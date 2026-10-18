#pragma once
#ifndef WMOD_00047_WINDHAWK_SYMBOL_COMPAT_H
#define WMOD_00047_WINDHAWK_SYMBOL_COMPAT_H

// Bounded source-level compatibility for Windhawk's Wh_Find*Symbol APIs.
// This uses the Windows DbgHelp symbol engine, not a hook/trampoline engine.
// DbgHelp has process-global state and is not generally thread-safe; calls
// made through this adapter serialize using a process-named mutex. The session
// intentionally is not cleaned up because another mod may share the process
// DbgHelp session.
#include <Windows.h>
#include <DbgHelp.h>
#include <Psapi.h>

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cwchar>
#include <filesystem>
#include <mutex>
#include <string>
#include <utility>
#include <vector>

#pragma comment(lib, "dbghelp.lib")
#pragma comment(lib, "psapi.lib")

#ifndef WMOD_WINDHAWK_FIND_SYMBOL_TYPES
#define WMOD_WINDHAWK_FIND_SYMBOL_TYPES
typedef struct tagWH_FIND_SYMBOL_OPTIONS {
    // Must equal sizeof(WH_FIND_SYMBOL_OPTIONS).
    size_t optionsSize;
    // NULL selects Microsoft's public symbol server. A URL can be supplied,
    // or a DbgHelp-compatible semicolon-separated search path.
    PCWSTR symbolServer;
    // Avoid name undecoration when TRUE; symbolDecorated remains available.
    BOOL noUndecoratedSymbols;
} WH_FIND_SYMBOL_OPTIONS;

typedef struct tagWH_FIND_SYMBOL {
    void* address;
    PCWSTR symbol;
    PCWSTR symbolDecorated;
} WH_FIND_SYMBOL;
#endif

namespace wmod::windhawk_symbol_compat {
constexpr size_t kMaxSearchPathChars = 4096;
constexpr size_t kMaxSymbolNameChars = 4096;
constexpr size_t kMaxSymbolCount = 100000;
constexpr size_t kMaxStoredNameChars = 4 * 1024 * 1024;
constexpr DWORD kDbgHelpLockTimeoutMs = 10000;

struct SymbolRecord {
    void* address = nullptr;
    std::wstring undecorated;
    std::wstring decorated;
};

struct SearchContext {
    std::vector<SymbolRecord> records;
    size_t index = 0;
    size_t storedNameChars = 0;
    bool noUndecoratedSymbols = false;
    bool truncated = false;
    bool callbackFailed = false;
};

inline std::wstring DefaultSearchPath() {
    wchar_t baseBuffer[32768]{};
    DWORD length = GetEnvironmentVariableW(L"LOCALAPPDATA", baseBuffer, _countof(baseBuffer));
    if (!length || length >= _countof(baseBuffer)) {
        length = GetTempPathW(_countof(baseBuffer), baseBuffer);
    }
    std::wstring cache;
    if (length && length < _countof(baseBuffer)) {
        std::filesystem::path cachePath(std::wstring(baseBuffer, length));
        cachePath /= L"00047";
        cachePath /= L"symbol-cache";
        std::error_code ec;
        std::filesystem::create_directories(cachePath, ec);
        if (!ec) cache = cachePath.wstring();
    }
    if (cache.empty()) {
        return L"srv*https://msdl.microsoft.com/download/symbols";
    }
    return L"srv*" + cache + L"*https://msdl.microsoft.com/download/symbols";
}

inline bool ResolveSearchPath(const WH_FIND_SYMBOL_OPTIONS* options,
    std::wstring& searchPath, bool& noUndecorated) noexcept {
    try {
        noUndecorated = FALSE;
        if (!options) {
            searchPath = DefaultSearchPath();
            return true;
        }
        if (options->optionsSize != sizeof(WH_FIND_SYMBOL_OPTIONS)) return false;
        noUndecorated = options->noUndecoratedSymbols != FALSE;
        if (!options->symbolServer) {
            searchPath = DefaultSearchPath();
            return true;
        }

        size_t length = 0;
        while (length <= kMaxSearchPathChars && options->symbolServer[length] != L'\0') ++length;
        if (length > kMaxSearchPathChars) return false;
        std::wstring requested(options->symbolServer, length);
        // A bare URL is treated as a symbol-server URL. A DbgHelp path (srv*,
        // cache expressions, or a semicolon-separated list) is passed through.
        const bool isUrl = requested.rfind(L"https://", 0) == 0 ||
            requested.rfind(L"http://", 0) == 0;
        if (isUrl) {
            std::wstring cachePath;
            wchar_t baseBuffer[32768]{};
            DWORD envLength = GetEnvironmentVariableW(L"LOCALAPPDATA", baseBuffer, _countof(baseBuffer));
            if (envLength && envLength < _countof(baseBuffer)) {
                std::filesystem::path path(std::wstring(baseBuffer, envLength));
                path /= L"00047";
                path /= L"symbol-cache";
                std::error_code ec;
                std::filesystem::create_directories(path, ec);
                if (!ec) cachePath = path.wstring();
            }
            searchPath = cachePath.empty()
                ? L"srv*" + requested
                : L"srv*" + cachePath + L"*" + requested;
        } else {
            searchPath = std::move(requested);
        }
        return true;
    } catch (...) {
        searchPath.clear();
        return false;
    }
}

inline HANDLE DbgHelpMutex() noexcept {
    wchar_t name[96]{};
    if (swprintf_s(name, _countof(name), L"Local\\00047.WindhawkSymbols.%lu",
            GetCurrentProcessId()) <= 0) return nullptr;
    return CreateMutexW(nullptr, FALSE, name);
}

class DbgHelpLock final {
public:
    DbgHelpLock() noexcept {
        mutex_ = DbgHelpMutex();
        if (!mutex_) return;
        const DWORD result = WaitForSingleObject(mutex_, kDbgHelpLockTimeoutMs);
        if (result == WAIT_OBJECT_0) {
            acquired_ = true;
        } else if (result == WAIT_ABANDONED) {
            // A caller died while DbgHelp was active; its session may be in an
            // unknown state, so do not continue symbol operations.
            ReleaseMutex(mutex_);
        } else {
            CloseHandle(mutex_);
            mutex_ = nullptr;
        }
    }
    ~DbgHelpLock() {
        if (acquired_) ReleaseMutex(mutex_);
        if (mutex_) CloseHandle(mutex_);
    }
    bool Acquired() const noexcept { return acquired_; }
    DbgHelpLock(const DbgHelpLock&) = delete;
    DbgHelpLock& operator=(const DbgHelpLock&) = delete;
private:
    HANDLE mutex_ = nullptr;
    bool acquired_ = false;
};

inline bool EnsureSession(const std::wstring& searchPath) noexcept {
    HANDLE process = GetCurrentProcess();
    const DWORD existingOptions = SymGetOptions();
    SymSetOptions(existingOptions | SYMOPT_DEFERRED_LOADS |
        SYMOPT_FAIL_CRITICAL_ERRORS | SYMOPT_NO_PROMPTS | SYMOPT_CASE_INSENSITIVE);
    SetLastError(ERROR_SUCCESS);
    if (!SymInitializeW(process, searchPath.c_str(), FALSE)) {
        // ERROR_INVALID_PARAMETER is what DbgHelp reports when this process
        // already has a symbol session. Reuse it instead of calling SymCleanup.
        if (GetLastError() != ERROR_INVALID_PARAMETER) {
            SymSetOptions(existingOptions);
            return false;
        }
    }
    if (!SymSetSearchPathW(process, searchPath.c_str())) {
        SymSetOptions(existingOptions);
        return false;
    }
    return true;
}

inline std::wstring ModuleImagePath(HMODULE module) {
    std::vector<wchar_t> buffer(32768);
    const DWORD length = GetModuleFileNameW(module, buffer.data(),
        static_cast<DWORD>(buffer.size()));
    if (!length || length >= buffer.size()) return {};
    return std::wstring(buffer.data(), length);
}

inline BOOL CALLBACK CollectSymbol(PSYMBOL_INFOW info, ULONG, PVOID user) noexcept {
    auto* context = static_cast<SearchContext*>(user);
    if (!context || !info) return TRUE;
    try {
        if (context->records.size() >= kMaxSymbolCount ||
            info->NameLen > kMaxSymbolNameChars ||
            context->storedNameChars + info->NameLen > kMaxStoredNameChars) {
            context->truncated = true;
            return FALSE;
        }
        std::wstring decorated(info->Name, info->NameLen);
        if (decorated.empty()) return TRUE;

        std::wstring undecorated;
        if (!context->noUndecoratedSymbols) {
            std::vector<wchar_t> buffer(kMaxSymbolNameChars + 1, L'\0');
            const DWORD length = UnDecorateSymbolNameW(decorated.c_str(),
                buffer.data(), static_cast<DWORD>(buffer.size()), UNDNAME_COMPLETE);
            if (length) undecorated.assign(buffer.data(), length);
            else undecorated = decorated;
        }

        SymbolRecord record;
        record.address = reinterpret_cast<void*>(static_cast<ULONG_PTR>(info->Address));
        record.undecorated = std::move(undecorated);
        record.decorated = std::move(decorated);
        context->storedNameChars += info->NameLen;
        if (!record.undecorated.empty()) context->storedNameChars += record.undecorated.size();
        context->records.push_back(std::move(record));
        return TRUE;
    } catch (...) {
        context->callbackFailed = true;
        return FALSE;
    }
}

inline void FillFindData(const SearchContext& state, WH_FIND_SYMBOL* findData) noexcept {
    const auto& record = state.records[state.index];
    findData->address = record.address;
    findData->symbol = record.undecorated.empty() ? nullptr : record.undecorated.c_str();
    findData->symbolDecorated = record.decorated.empty() ? nullptr : record.decorated.c_str();
}

inline HANDLE FindFirst(HMODULE module, const WH_FIND_SYMBOL_OPTIONS* options,
    WH_FIND_SYMBOL* findData) {
    if (!findData) return nullptr;
    findData->address = nullptr;
    findData->symbol = nullptr;
    findData->symbolDecorated = nullptr;

    std::wstring searchPath;
    bool noUndecorated = false;
    if (!ResolveSearchPath(options, searchPath, noUndecorated)) return nullptr;

    if (!module) module = GetModuleHandleW(nullptr);
    if (!module) return nullptr;
    MODULEINFO moduleInfo{};
    if (!GetModuleInformation(GetCurrentProcess(), module, &moduleInfo, sizeof(moduleInfo)) ||
        !moduleInfo.lpBaseOfDll || !moduleInfo.SizeOfImage) return nullptr;
    const std::wstring imagePath = ModuleImagePath(module);
    if (imagePath.empty()) return nullptr;

    try {
        SearchContext* state = new SearchContext();
        state->noUndecoratedSymbols = noUndecorated;
        DbgHelpLock lock;
        if (!lock.Acquired()) {
            delete state;
            return nullptr;
        }

        const DWORD existingOptions = SymGetOptions();
        if (!EnsureSession(searchPath)) {
            delete state;
            return nullptr;
        }

        const DWORD64 moduleBase = static_cast<DWORD64>(
            reinterpret_cast<ULONG_PTR>(moduleInfo.lpBaseOfDll));
        SetLastError(ERROR_SUCCESS);
        DWORD64 loadedBase = SymLoadModuleExW(GetCurrentProcess(), nullptr,
            imagePath.c_str(), nullptr, moduleBase, moduleInfo.SizeOfImage, nullptr, 0);
        if (!loadedBase) {
            // DbgHelp returns zero with ERROR_SUCCESS when the module is already
            // loaded in the current symbol session.
            if (GetLastError() != ERROR_SUCCESS) {
                SymSetOptions(existingOptions);
                delete state;
                return nullptr;
            }
            loadedBase = moduleBase;
        }

        const BOOL enumerated = SymEnumSymbolsW(GetCurrentProcess(), loadedBase,
            L"*", CollectSymbol, state);
        SymSetOptions(existingOptions);
        if ((!enumerated && !state->truncated) || state->callbackFailed ||
            state->records.empty()) {
            delete state;
            return nullptr;
        }
        FillFindData(*state, findData);
        return reinterpret_cast<HANDLE>(state);
    } catch (...) {
        return nullptr;
    }
}
} // namespace wmod::windhawk_symbol_compat

// Symbols are snapshotted before returning. Returned string pointers remain
// valid until the next Wh_FindNextSymbol call on that handle or close.
inline HANDLE Wh_FindFirstSymbol(HMODULE module,
    const WH_FIND_SYMBOL_OPTIONS* options, WH_FIND_SYMBOL* findData) {
    try {
        return ::wmod::windhawk_symbol_compat::FindFirst(module, options, findData);
    } catch (...) {
        if (findData) {
            findData->address = nullptr;
            findData->symbol = nullptr;
            findData->symbolDecorated = nullptr;
        }
        return nullptr;
    }
}

inline BOOL Wh_FindNextSymbol(HANDLE symSearch, WH_FIND_SYMBOL* findData) {
    if (!symSearch || !findData) return FALSE;
    auto* state = reinterpret_cast<::wmod::windhawk_symbol_compat::SearchContext*>(symSearch);
    if (state->index + 1 >= state->records.size()) {
        findData->address = nullptr;
        findData->symbol = nullptr;
        findData->symbolDecorated = nullptr;
        return FALSE;
    }
    ++state->index;
    ::wmod::windhawk_symbol_compat::FillFindData(*state, findData);
    return TRUE;
}

inline void Wh_FindCloseSymbol(HANDLE symSearch) {
    if (!symSearch) return;
    delete reinterpret_cast<::wmod::windhawk_symbol_compat::SearchContext*>(symSearch);
}

#endif // WMOD_00047_WINDHAWK_SYMBOL_COMPAT_H
