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
#include "windhawk_hook_compat.h"
#include "00047_mod_services.h"

#include <algorithm>
#include <atomic>
#include <cerrno>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <cwchar>
#include <cwctype>
#include <filesystem>
#include <limits>
#include <mutex>
#include <string>
#include <string_view>
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


// Source-level compatibility for Windhawk's symbol-selected hook API.
// Resolution is based on the bounded local DbgHelp snapshot; online cache,
// hybrid-module support, and cross-mod hook coordination are not implemented.
#ifndef WMOD_WINDHAWK_HOOK_SYMBOL_TYPES
#define WMOD_WINDHAWK_HOOK_SYMBOL_TYPES
typedef struct tagWH_SYMBOL_HOOK {
    const struct {
        PCWSTR string;
        size_t length;
    }* symbols;
    size_t symbolsCount;
    void** pOriginalFunction;
    void* hookFunction;
    bool optional;
} WH_SYMBOL_HOOK;

typedef struct tagWH_HOOK_SYMBOLS_OPTIONS {
    size_t optionsSize;
    PCWSTR symbolServer;
    BOOL noUndecoratedSymbols;
    PCWSTR onlineCacheUrl;
} WH_HOOK_SYMBOLS_OPTIONS;
#endif


namespace wmod::windhawk_symbol_compat {
struct HookCacheIdentity {
    std::uintptr_t moduleBase = 0;
    size_t imageSize = 0;
    DWORD timestamp = 0;
    std::wstring moduleFileName;
    std::wstring headerModuleName;
    std::wstring version;
    std::wstring cacheKey;
};

struct HookCachedResolution {
    size_t index = 0;
    void* target = nullptr;
    std::wstring symbol;
};

inline std::atomic<unsigned long long>& HookCacheHitsCounter() noexcept {
    static std::atomic<unsigned long long> hits{0};
    return hits;
}

inline unsigned long long HookCacheHitCount() noexcept {
    return HookCacheHitsCounter().load(std::memory_order_relaxed);
}

inline std::uint64_t HashWide(std::wstring_view value,
    std::uint64_t hash = 14695981039346656037ull) noexcept {
    for (wchar_t c : value) {
        const auto code = static_cast<std::uint16_t>(c);
        hash ^= static_cast<unsigned char>(code & 0xff);
        hash *= 1099511628211ull;
        hash ^= static_cast<unsigned char>((code >> 8) & 0xff);
        hash *= 1099511628211ull;
    }
    return hash;
}

inline std::wstring HashText(std::uint64_t value) {
    wchar_t buffer[17]{};
    if (swprintf_s(buffer, _countof(buffer), L"%016llx",
            static_cast<unsigned long long>(value)) <= 0) return {};
    return buffer;
}

inline std::uint64_t HookSelectorsHash(
    const WH_SYMBOL_HOOK* hooks, size_t count, BOOL noUndecorated) noexcept {
    std::uint64_t hash = 14695981039346656037ull;
    auto addNumber = [&hash](std::uint64_t value) {
        for (unsigned i = 0; i < sizeof(value); ++i) {
            hash ^= static_cast<unsigned char>((value >> (i * 8)) & 0xff);
            hash *= 1099511628211ull;
        }
    };
    addNumber(noUndecorated != FALSE);
    addNumber(count);
    for (size_t i = 0; i < count; ++i) {
        addNumber(hooks[i].optional != false);
        addNumber(hooks[i].symbolsCount);
        for (size_t j = 0; j < hooks[i].symbolsCount; ++j) {
            const auto& selector = hooks[i].symbols[j];
            addNumber(selector.length);
            for (size_t k = 0; selector.string && k < selector.length; ++k) {
                const auto code = static_cast<std::uint16_t>(selector.string[k]);
                hash ^= static_cast<unsigned char>(code & 0xff);
                hash *= 1099511628211ull;
                hash ^= static_cast<unsigned char>((code >> 8) & 0xff);
                hash *= 1099511628211ull;
            }
            addNumber(0xffffffffffffffffull);
        }
    }
    return hash;
}

inline bool BuildHookCacheIdentity(
    HMODULE module, const MODULEINFO& moduleInfo,
    HookCacheIdentity& identity) noexcept {
    try {
        if (!module || !moduleInfo.lpBaseOfDll || !moduleInfo.SizeOfImage ||
            moduleInfo.SizeOfImage < sizeof(IMAGE_DOS_HEADER)) return false;
        const auto* base = static_cast<const BYTE*>(moduleInfo.lpBaseOfDll);
        IMAGE_DOS_HEADER dos{};
        std::memcpy(&dos, base, sizeof(dos));
        if (dos.e_magic != IMAGE_DOS_SIGNATURE || dos.e_lfanew <= 0) return false;
        const size_t ntOffset = static_cast<size_t>(dos.e_lfanew);
        if (ntOffset > moduleInfo.SizeOfImage ||
            sizeof(IMAGE_NT_HEADERS64) > moduleInfo.SizeOfImage - ntOffset) return false;
        IMAGE_NT_HEADERS64 nt{};
        std::memcpy(&nt, base + ntOffset, sizeof(nt));
        if (nt.Signature != IMAGE_NT_SIGNATURE ||
            nt.FileHeader.Machine != IMAGE_FILE_MACHINE_AMD64 ||
            nt.OptionalHeader.Magic != IMAGE_NT_OPTIONAL_HDR64_MAGIC ||
            nt.OptionalHeader.SizeOfImage != moduleInfo.SizeOfImage) return false;

        const std::wstring modulePath = ModuleImagePath(module);
        if (modulePath.empty()) return false;
        std::wstring fileName = std::filesystem::path(modulePath).filename().wstring();
        if (fileName.empty()) return false;
        std::transform(fileName.begin(), fileName.end(), fileName.begin(),
            [](wchar_t c) { return static_cast<wchar_t>(std::towlower(c)); });

        identity.moduleBase = reinterpret_cast<std::uintptr_t>(moduleInfo.lpBaseOfDll);
        identity.imageSize = moduleInfo.SizeOfImage;
        identity.timestamp = nt.FileHeader.TimeDateStamp;
        identity.moduleFileName = std::move(fileName);
        identity.headerModuleName.clear();
        for (wchar_t c : identity.moduleFileName) {
            if (c == L'#') identity.headerModuleName += L"%sep%";
            else identity.headerModuleName.push_back(c);
        }
        identity.version = std::to_wstring(identity.timestamp) + L"-" +
            std::to_wstring(identity.imageSize);
        // PE timestamp + image size + lowercase image name is Windhawk's
        // documented fallback identity when a module has no usable PDB GUID.
        // This local cache deliberately uses that conservative identity even for
        // PDB-backed images; a cache miss simply falls back to DbgHelp.
        identity.cacheKey = L"pe_x86-64_";
        identity.cacheKey += std::to_wstring(identity.timestamp);
        identity.cacheKey += L'_';
        identity.cacheKey += std::to_wstring(identity.imageSize);
        identity.cacheKey += L'_';
        identity.cacheKey += identity.moduleFileName;
        return true;
    } catch (...) {
        return false;
    }
}

inline std::filesystem::path HookCachePath(
    const HookCacheIdentity& identity, const WH_SYMBOL_HOOK* hooks,
    size_t count, BOOL noUndecorated) noexcept {
    try {
        wchar_t localAppData[32768]{};
        const DWORD length = GetEnvironmentVariableW(
            L"LOCALAPPDATA", localAppData, static_cast<DWORD>(_countof(localAppData)));
        if (!length || length >= _countof(localAppData)) return {};

        std::wstring scope;
        const auto package = wmod::PackageDirectory();
        if (!package.empty()) scope = package.filename().wstring();
        if (scope.empty()) {
            HMODULE owner = nullptr;
            if (GetModuleHandleExW(
                    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS |
                        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                    reinterpret_cast<LPCWSTR>(&HookCachePath), &owner) && owner) {
                scope = ModuleImagePath(owner);
            }
        }
        if (scope.empty()) return {};

        std::filesystem::path root(localAppData);
        root /= L"00047";
        root /= L"symbol-cache";
        root /= L"hook-offsets";
        std::error_code ec;
        std::filesystem::create_directories(root, ec);
        if (ec || !std::filesystem::is_directory(root, ec) || ec) return {};
        const std::wstring fileName =
            HashText(HashWide(scope)) + L"_" +
            HashText(HashWide(identity.cacheKey)) + L"_" +
            HashText(HookSelectorsHash(hooks, count, noUndecorated)) + L".txt";
        return root / fileName;
    } catch (...) {
        return {};
    }
}

inline bool HookCacheExecutableTarget(
    const HookCacheIdentity& identity, std::uint64_t offset,
    void*& target) noexcept {
    target = nullptr;
    if (offset >= identity.imageSize ||
        identity.moduleBase > std::numeric_limits<std::uintptr_t>::max() - offset) return false;
    const auto address = identity.moduleBase + static_cast<std::uintptr_t>(offset);
    MEMORY_BASIC_INFORMATION region{};
    if (VirtualQuery(reinterpret_cast<const void*>(address), &region, sizeof(region)) != sizeof(region) ||
        region.State != MEM_COMMIT ||
        region.AllocationBase != reinterpret_cast<void*>(identity.moduleBase) ||
        (region.Protect & (PAGE_GUARD | PAGE_NOACCESS)) != 0) return false;
    const DWORD protection = region.Protect & 0xff;
    if (protection != PAGE_EXECUTE && protection != PAGE_EXECUTE_READ &&
        protection != PAGE_EXECUTE_READWRITE && protection != PAGE_EXECUTE_WRITECOPY) return false;
    target = reinterpret_cast<void*>(address);
    return true;
}

inline bool ReadLocalHookCache(
    const std::filesystem::path& path, std::wstring& cacheText) noexcept {
    cacheText.clear();
    try {
        if (path.empty()) return false;
        HANDLE file = CreateFileW(path.c_str(), GENERIC_READ, FILE_SHARE_READ,
            nullptr, OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT, nullptr);
        if (file == INVALID_HANDLE_VALUE) return false;
        BY_HANDLE_FILE_INFORMATION info{};
        LARGE_INTEGER size{};
        const bool valid =
            GetFileInformationByHandle(file, &info) != FALSE &&
            (info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) == 0 &&
            GetFileSizeEx(file, &size) != FALSE && size.QuadPart > 0 &&
            size.QuadPart <= 4 * 1024 * 1024;
        if (!valid) {
            CloseHandle(file);
            return false;
        }
        std::string utf8(static_cast<size_t>(size.QuadPart), '\0');
        size_t offset = 0;
        while (offset < utf8.size()) {
            DWORD read = 0;
            const DWORD request = static_cast<DWORD>(
                std::min<size_t>(utf8.size() - offset, 64 * 1024));
            if (!ReadFile(file, utf8.data() + offset, request, &read, nullptr) || read == 0) {
                CloseHandle(file);
                return false;
            }
            offset += read;
        }
        CloseHandle(file);
        const int chars = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS,
            utf8.data(), static_cast<int>(utf8.size()), nullptr, 0);
        if (chars <= 0) return false;
        cacheText.resize(static_cast<size_t>(chars));
        if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS,
                utf8.data(), static_cast<int>(utf8.size()), cacheText.data(), chars) != chars) {
            cacheText.clear();
            return false;
        }
        return true;
    } catch (...) {
        cacheText.clear();
        return false;
    }
}

inline bool WriteLocalHookCache(
    const std::filesystem::path& path, std::wstring_view cacheText) noexcept {
    std::filesystem::path temporary;
    HANDLE file = INVALID_HANDLE_VALUE;
    try {
        if (path.empty() || cacheText.empty() || cacheText.size() > 4 * 1024 * 1024) return false;
        const int bytes = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS,
            cacheText.data(), static_cast<int>(cacheText.size()), nullptr, 0, nullptr, nullptr);
        if (bytes <= 0 || bytes > 4 * 1024 * 1024) return false;
        std::string utf8(static_cast<size_t>(bytes), '\0');
        if (WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS,
                cacheText.data(), static_cast<int>(cacheText.size()), utf8.data(),
                bytes, nullptr, nullptr) != bytes) return false;

        temporary = path;
        temporary += L".tmp." + std::to_wstring(GetCurrentProcessId()) + L"." +
            std::to_wstring(GetCurrentThreadId()) + L"." + std::to_wstring(GetTickCount64());
        file = CreateFileW(temporary.c_str(), GENERIC_WRITE, 0, nullptr,
            CREATE_NEW, FILE_ATTRIBUTE_TEMPORARY, nullptr);
        if (file == INVALID_HANDLE_VALUE) return false;
        size_t offset = 0;
        while (offset < utf8.size()) {
            DWORD written = 0;
            const DWORD request = static_cast<DWORD>(
                std::min<size_t>(utf8.size() - offset, 64 * 1024));
            if (!WriteFile(file, utf8.data() + offset, request, &written, nullptr) || written == 0) {
                CloseHandle(file);
                file = INVALID_HANDLE_VALUE;
                DeleteFileW(temporary.c_str());
                return false;
            }
            offset += written;
        }
        if (!FlushFileBuffers(file)) {
            CloseHandle(file);
            file = INVALID_HANDLE_VALUE;
            DeleteFileW(temporary.c_str());
            return false;
        }
        CloseHandle(file);
        file = INVALID_HANDLE_VALUE;
        if (!MoveFileExW(temporary.c_str(), path.c_str(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)) {
            DeleteFileW(temporary.c_str());
            return false;
        }
        return true;
    } catch (...) {
        if (file != INVALID_HANDLE_VALUE) CloseHandle(file);
        if (!temporary.empty()) DeleteFileW(temporary.c_str());
        return false;
    }
}

inline std::wstring HookCacheText(
    const HookCacheIdentity& identity, const WH_SYMBOL_HOOK* hooks,
    size_t count, const std::vector<bool>& found,
    const std::vector<HookCachedResolution>& resolved) {
    std::wstring text = L"1#" + identity.headerModuleName + L"#" + identity.version;
    for (const auto& item : resolved) {
        if (item.symbol.empty() || item.symbol.find(L'#') != std::wstring::npos ||
            reinterpret_cast<std::uintptr_t>(item.target) < identity.moduleBase) return {};
        const auto offset = reinterpret_cast<std::uintptr_t>(item.target) - identity.moduleBase;
        if (offset >= identity.imageSize) return {};
        text += L"#" + item.symbol + L"#" + std::to_wstring(offset);
    }
    for (size_t i = 0; i < count; ++i) {
        if (found[i] || !hooks[i].optional) continue;
        for (size_t j = 0; j < hooks[i].symbolsCount; ++j) {
            const auto& selector = hooks[i].symbols[j];
            const std::wstring_view symbol(selector.string, selector.length);
            if (symbol.empty() || symbol.find(L'#') != std::wstring_view::npos) continue;
            text += L"#";
            text.append(symbol);
            text += L"#";
        }
    }
    return text.size() <= 4 * 1024 * 1024 ? text : std::wstring{};
}

inline bool ResolveHookCacheText(
    std::wstring_view cacheText, const HookCacheIdentity& identity,
    const WH_SYMBOL_HOOK* hooks, size_t count, std::vector<bool>& found,
    std::vector<HookCachedResolution>& resolved) noexcept {
    found.clear();
    resolved.clear();
    try {
        if (cacheText.empty() || cacheText.size() > 4 * 1024 * 1024) return false;
        std::vector<std::wstring_view> fields;
        fields.reserve(64);
        size_t start = 0;
        for (;;) {
            const size_t separator = cacheText.find(L'#', start);
            if (separator == std::wstring_view::npos) {
                fields.push_back(cacheText.substr(start));
                break;
            }
            fields.push_back(cacheText.substr(start, separator - start));
            start = separator + 1;
            if (fields.size() > 100000) return false;
        }
        if (fields.size() < 3 || fields[0] != L"1" ||
            fields[1] != identity.headerModuleName || fields[2] != identity.version ||
            ((fields.size() - 3) % 2) != 0) return false;

        struct Entry { std::wstring symbol; void* target = nullptr; };
        std::vector<Entry> entries;
        entries.reserve((fields.size() - 3) / 2);
        for (size_t i = 3; i + 1 < fields.size(); i += 2) {
            if (fields[i].empty() || fields[i].size() > kMaxSymbolNameChars) return false;
            Entry entry;
            entry.symbol.assign(fields[i]);
            if (!fields[i + 1].empty()) {
                for (wchar_t c : fields[i + 1]) {
                    if (c < L'0' || c > L'9') return false;
                }
                const std::wstring offsetText(fields[i + 1]);
                errno = 0;
                wchar_t* end = nullptr;
                const unsigned long long offset = std::wcstoull(offsetText.c_str(), &end, 10);
                if (errno == ERANGE || end == offsetText.c_str() || !end || *end != L'\0' ||
                    !HookCacheExecutableTarget(identity, offset, entry.target)) return false;
            }
            entries.push_back(std::move(entry));
        }

        std::vector<bool> stagedFound(count, false);
        std::vector<HookCachedResolution> staged;
        staged.reserve(count);
        for (const auto& entry : entries) {
            if (!entry.target) continue;
            for (size_t i = 0; i < count; ++i) {
                if (stagedFound[i]) continue;
                for (size_t j = 0; j < hooks[i].symbolsCount; ++j) {
                    const auto& selector = hooks[i].symbols[j];
                    if (std::wstring_view(selector.string, selector.length) != entry.symbol) continue;
                    stagedFound[i] = true;
                    staged.push_back({i, entry.target, entry.symbol});
                    break;
                }
            }
        }
        for (size_t i = 0; i < count; ++i) {
            if (!stagedFound[i] && !hooks[i].optional) return false;
        }
        found = std::move(stagedFound);
        resolved = std::move(staged);
        return true;
    } catch (...) {
        found.clear();
        resolved.clear();
        return false;
    }
}
} // namespace wmod::windhawk_symbol_compat


inline BOOL Wh_HookSymbols(HMODULE module,
    const WH_SYMBOL_HOOK* symbolHooks,
    size_t symbolHooksCount,
    const WH_HOOK_SYMBOLS_OPTIONS* options) {
    if (!module) return FALSE;
    if (symbolHooksCount == 0) return TRUE;
    if (!symbolHooks || symbolHooksCount > 2048) return FALSE;

    struct HookOptionsV1 {
        size_t optionsSize;
        PCWSTR symbolServer;
        BOOL noUndecoratedSymbols;
    };
    PCWSTR symbolServer = nullptr;
    BOOL noUndecoratedSymbols = FALSE;
    if (options) {
        if (options->optionsSize == sizeof(WH_HOOK_SYMBOLS_OPTIONS)) {
            symbolServer = options->symbolServer;
            noUndecoratedSymbols = options->noUndecoratedSymbols;
        } else if (options->optionsSize == sizeof(HookOptionsV1)) {
            const auto* v1 = reinterpret_cast<const HookOptionsV1*>(options);
            symbolServer = v1->symbolServer;
            noUndecoratedSymbols = v1->noUndecoratedSymbols;
        } else if (options->optionsSize != 0) {
            return FALSE;
        }
    }

    std::vector<wmod::windhawk_symbol_compat::HookCachedResolution> resolved;
    std::vector<bool> found;
    try {
        resolved.reserve(symbolHooksCount);
        found.assign(symbolHooksCount, false);
        for (size_t i = 0; i < symbolHooksCount; ++i) {
            if (symbolHooks[i].symbolsCount > 512 ||
                (symbolHooks[i].symbolsCount != 0 && !symbolHooks[i].symbols)) return FALSE;
            for (size_t j = 0; j < symbolHooks[i].symbolsCount; ++j) {
                const auto& selector = symbolHooks[i].symbols[j];
                if (!selector.string || selector.length == 0 ||
                    selector.length > ::wmod::windhawk_symbol_compat::kMaxSymbolNameChars) return FALSE;
            }
        }
    } catch (...) {
        return FALSE;
    }

    MODULEINFO moduleInfo{};
    if (!GetModuleInformation(GetCurrentProcess(), module, &moduleInfo, sizeof(moduleInfo)) ||
        !moduleInfo.lpBaseOfDll || moduleInfo.SizeOfImage == 0) return FALSE;

    wmod::windhawk_symbol_compat::HookCacheIdentity cacheIdentity{};
    const bool identityAvailable =
        wmod::windhawk_symbol_compat::BuildHookCacheIdentity(module, moduleInfo, cacheIdentity);
    const auto cachePath = identityAvailable
        ? wmod::windhawk_symbol_compat::HookCachePath(
            cacheIdentity, symbolHooks, symbolHooksCount, noUndecoratedSymbols)
        : std::filesystem::path{};
    bool cacheHit = false;
    if (!cachePath.empty()) {
        std::wstring cached;
        if (wmod::windhawk_symbol_compat::ReadLocalHookCache(cachePath, cached) &&
            wmod::windhawk_symbol_compat::ResolveHookCacheText(
                cached, cacheIdentity, symbolHooks, symbolHooksCount, found, resolved)) {
            cacheHit = true;
            wmod::windhawk_symbol_compat::HookCacheHitsCounter().fetch_add(1, std::memory_order_relaxed);
        }
    }

    if (!cacheHit) {
        WH_FIND_SYMBOL_OPTIONS findOptions{};
        findOptions.optionsSize = sizeof(findOptions);
        findOptions.symbolServer = symbolServer;
        findOptions.noUndecoratedSymbols = noUndecoratedSymbols;
        WH_FIND_SYMBOL current{};
        HANDLE search = Wh_FindFirstSymbol(module, &findOptions, &current);
        if (!search) {
            for (size_t i = 0; i < symbolHooksCount; ++i) {
                if (!symbolHooks[i].optional) return FALSE;
            }
        } else {
            try {
                do {
                    const std::wstring_view undecorated =
                        current.symbol ? std::wstring_view(current.symbol) : std::wstring_view();
                    const std::wstring_view decorated =
                        current.symbolDecorated ? std::wstring_view(current.symbolDecorated) : std::wstring_view();
                    for (size_t i = 0; i < symbolHooksCount; ++i) {
                        if (found[i]) continue;
                        const auto& hook = symbolHooks[i];
                        for (size_t j = 0; j < hook.symbolsCount; ++j) {
                            const auto& selector = hook.symbols[j];
                            const std::wstring_view wanted(selector.string, selector.length);
                            if (wanted == undecorated || wanted == decorated) {
                                found[i] = true;
                                resolved.push_back({i, current.address, std::wstring(wanted)});
                                break;
                            }
                        }
                        if (found[i]) break;
                    }
                } while (Wh_FindNextSymbol(search, &current));
            } catch (...) {
                Wh_FindCloseSymbol(search);
                return FALSE;
            }
            Wh_FindCloseSymbol(search);
        }
        for (size_t i = 0; i < symbolHooksCount; ++i) {
            if (!found[i] && !symbolHooks[i].optional) return FALSE;
        }
    }

    struct RegisteredHook {
        void* target = nullptr;
        void** originalSlot = nullptr;
        void* previousOriginal = nullptr;
    };
    std::vector<RegisteredHook> registered;
    try {
        registered.reserve(resolved.size());
        for (const auto& match : resolved) {
            const auto& hook = symbolHooks[match.index];
            if (!hook.hookFunction) continue;
            void* previousOriginal = hook.pOriginalFunction ? *hook.pOriginalFunction : nullptr;
            if (!Wh_SetFunctionHook(match.target, hook.hookFunction, hook.pOriginalFunction)) {
                if (hook.pOriginalFunction) *hook.pOriginalFunction = previousOriginal;
                for (auto it = registered.rbegin(); it != registered.rend(); ++it) {
                    (void)Wh_RemoveFunctionHook(it->target);
                    if (it->originalSlot) *it->originalSlot = it->previousOriginal;
                }
                return FALSE;
            }
            registered.push_back({match.target, hook.pOriginalFunction, previousOriginal});
        }
        for (const auto& match : resolved) {
            const auto& hook = symbolHooks[match.index];
            if (!hook.hookFunction && hook.pOriginalFunction) *hook.pOriginalFunction = match.target;
        }

        if (!cacheHit && identityAvailable && !cachePath.empty()) {
            try {
                const auto cacheText = wmod::windhawk_symbol_compat::HookCacheText(
                    cacheIdentity, symbolHooks, symbolHooksCount, found, resolved);
                if (!cacheText.empty()) {
                    (void)wmod::windhawk_symbol_compat::WriteLocalHookCache(cachePath, cacheText);
                }
            } catch (...) {
                // Cache I/O is opportunistic and never changes hook registration success.
            }
        }
        return TRUE;
    } catch (...) {
        for (auto it = registered.rbegin(); it != registered.rend(); ++it) {
            (void)Wh_RemoveFunctionHook(it->target);
            if (it->originalSlot) *it->originalSlot = it->previousOriginal;
        }
        return FALSE;
    }
}

#endif // WMOD_00047_WINDHAWK_SYMBOL_COMPAT_H
