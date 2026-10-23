#include "windhawk_hook_compat.h"

#if !defined(_M_X64) && !defined(__x86_64__)
#error The bundled Windhawk-compatible hook backend supports x64 only.
#endif

#include "third_party/minhook/include/MinHook.h"
#include "third_party/distorm/include/distorm.h"

#include <algorithm>
#include <array>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <cwchar>
#include <mutex>
#include <TlHelp32.h>
#include <Psapi.h>
#include <limits>
#include <string>
#include <unordered_map>
#include <vector>

#pragma comment(lib, "psapi.lib")

namespace {
constexpr DWORD kSharedRegistryMagic = 0x57484D31u;
constexpr DWORD kSharedRegistryVersion = 1;
constexpr DWORD kSharedRegistryWaitMs = 5000;
constexpr size_t kMaxSharedHookEntries = 4096;

struct SharedHookEntry {
    std::uintptr_t target = 0;
    std::uintptr_t owner = 0;
};

struct SharedHookTable {
    DWORD magic = 0;
    DWORD version = 0;
    DWORD poisoned = 0;
    DWORD count = 0;
    SharedHookEntry entries[kMaxSharedHookEntries]{};
};

// Each mod DLL has its own C++ statics, so coordinate hook target ownership via
// a per-process named mutex and shared mapping. A stale/abandoned registry is
// deliberately fail-closed: new hooks are refused and unload remains blocked.
class ProcessHookRegistry final {
public:
    ProcessHookRegistry() = default;
    ProcessHookRegistry(const ProcessHookRegistry&) = delete;
    ProcessHookRegistry& operator=(const ProcessHookRegistry&) = delete;

    ~ProcessHookRegistry() {
        if (table_) UnmapViewOfFile(table_);
        if (mapping_) CloseHandle(mapping_);
        if (mutex_) CloseHandle(mutex_);
    }

    bool Claim(void* target, HMODULE owner) noexcept {
        if (!target || !owner || !LockTable()) return false;
        bool success = false;
        const auto targetValue = reinterpret_cast<std::uintptr_t>(target);
        const auto ownerValue = reinterpret_cast<std::uintptr_t>(owner);
        bool alreadyClaimed = false;
        for (DWORD i = 0; i < table_->count; ++i) {
            if (table_->entries[i].target == targetValue) {
                alreadyClaimed = true;
                break;
            }
        }
        if (!alreadyClaimed && table_->count < kMaxSharedHookEntries) {
            table_->entries[table_->count] = {targetValue, ownerValue};
            ++table_->count;
            success = true;
        }
        ReleaseTable();
        return success;
    }

    bool Release(void* target, HMODULE owner) noexcept {
        if (!target || !owner || !LockTable()) return false;
        bool success = false;
        const auto targetValue = reinterpret_cast<std::uintptr_t>(target);
        const auto ownerValue = reinterpret_cast<std::uintptr_t>(owner);
        for (DWORD i = 0; i < table_->count; ++i) {
            if (table_->entries[i].target != targetValue) continue;
            if (table_->entries[i].owner != ownerValue) break;
            table_->entries[i] = table_->entries[table_->count - 1];
            --table_->count;
            table_->entries[table_->count] = {};
            success = true;
            break;
        }
        ReleaseTable();
        return success;
    }

    bool ReferencesModule(HMODULE module) noexcept {
        if (!module || !LockTable()) return true;
        MODULEINFO info{};
        if (!GetModuleInformation(GetCurrentProcess(), module, &info, sizeof(info)) ||
            !info.lpBaseOfDll || info.SizeOfImage == 0) {
            ReleaseTable();
            return true; // Fail closed when module bounds are unavailable.
        }
        const auto start = reinterpret_cast<std::uintptr_t>(info.lpBaseOfDll);
        const auto end = start + info.SizeOfImage;
        if (end < start) {
            ReleaseTable();
            return true;
        }
        const auto ownerValue = reinterpret_cast<std::uintptr_t>(module);
        bool found = false;
        for (DWORD i = 0; i < table_->count; ++i) {
            const auto target = table_->entries[i].target;
            if (table_->entries[i].owner == ownerValue || (target >= start && target < end)) {
                found = true;
                break;
            }
        }
        ReleaseTable();
        return found;
    }

private:
    bool EnsureReady() noexcept {
        try {
            std::lock_guard<std::mutex> guard(initMutex_);
            if (attempted_) return ready_;
            attempted_ = true;

            wchar_t mutexName[96]{};
            wchar_t mappingName[96]{};
            const auto pid = static_cast<unsigned long>(GetCurrentProcessId());
            if (swprintf_s(mutexName, _countof(mutexName),
                    L"Local\\00047NativeHookRegistryLock-%lu", pid) < 0 ||
                swprintf_s(mappingName, _countof(mappingName),
                    L"Local\\00047NativeHookRegistry-%lu", pid) < 0) {
                return false;
            }

            HANDLE mutex = CreateMutexW(nullptr, FALSE, mutexName);
            if (!mutex) return false;
            HANDLE mapping = CreateFileMappingW(
                INVALID_HANDLE_VALUE, nullptr, PAGE_READWRITE, 0,
                static_cast<DWORD>(sizeof(SharedHookTable)), mappingName);
            if (!mapping) {
                CloseHandle(mutex);
                return false;
            }
            const bool existed = GetLastError() == ERROR_ALREADY_EXISTS;
            auto* table = static_cast<SharedHookTable*>(
                MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, sizeof(SharedHookTable)));
            if (!table) {
                CloseHandle(mapping);
                CloseHandle(mutex);
                return false;
            }

            const DWORD wait = WaitForSingleObject(mutex, kSharedRegistryWaitMs);
            if (wait == WAIT_ABANDONED) {
                if (table->magic == kSharedRegistryMagic) table->poisoned = 1;
                ReleaseMutex(mutex);
                UnmapViewOfFile(table);
                CloseHandle(mapping);
                CloseHandle(mutex);
                return false;
            }
            if (wait != WAIT_OBJECT_0) {
                UnmapViewOfFile(table);
                CloseHandle(mapping);
                CloseHandle(mutex);
                return false;
            }

            if (!existed) {
                std::memset(table, 0, sizeof(*table));
                table->magic = kSharedRegistryMagic;
                table->version = kSharedRegistryVersion;
            } else if (table->magic != kSharedRegistryMagic ||
                table->version != kSharedRegistryVersion ||
                table->count > kMaxSharedHookEntries) {
                table->poisoned = 1;
            }
            ReleaseMutex(mutex);

            mutex_ = mutex;
            mapping_ = mapping;
            table_ = table;
            ready_ = true;
            return true;
        } catch (...) {
            return false;
        }
    }

    bool LockTable() noexcept {
        if (!EnsureReady()) return false;
        const DWORD wait = WaitForSingleObject(mutex_, kSharedRegistryWaitMs);
        if (wait == WAIT_ABANDONED) {
            if (table_) table_->poisoned = 1;
            ReleaseMutex(mutex_);
            return false;
        }
        if (wait != WAIT_OBJECT_0) return false;

        if (!table_ || table_->magic != kSharedRegistryMagic ||
            table_->version != kSharedRegistryVersion ||
            table_->poisoned != 0 || table_->count > kMaxSharedHookEntries) {
            if (table_) table_->poisoned = 1;
            ReleaseMutex(mutex_);
            return false;
        }
        return true;
    }

    void ReleaseTable() noexcept {
        if (mutex_) ReleaseMutex(mutex_);
    }

    std::mutex initMutex_;
    HANDLE mutex_ = nullptr;
    HANDLE mapping_ = nullptr;
    SharedHookTable* table_ = nullptr;
    bool attempted_ = false;
    bool ready_ = false;
};

ProcessHookRegistry& SharedHookRegistry() {
    static ProcessHookRegistry registry;
    return registry;
}

HMODULE CurrentHookOwnerModule() noexcept {
    HMODULE module = nullptr;
    if (!GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS |
                GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            reinterpret_cast<LPCWSTR>(&Wh_SetFunctionHook), &module)) {
        return nullptr;
    }
    return module;
}

struct HookRecord {
    void* target = nullptr;
    void* detour = nullptr;
    void* original = nullptr;
    bool enabled = false;
    bool removeQueued = false;
    HMODULE ownerModule = nullptr;
    bool registryClaimed = false;
    bool backendRemoved = false;
};

std::mutex g_mutex;
std::unordered_map<void*, HookRecord> g_hooks;
bool g_initialized = false;
bool g_accepting = true;

bool EnsureInitializedLocked() noexcept {
    if (g_initialized) return true;
    const MH_STATUS status = MH_Initialize();
    if (status != MH_OK && status != MH_ERROR_ALREADY_INITIALIZED) return false;
    g_initialized = true;
    return true;
}

void TryUninitializeLocked() noexcept {
    if (!g_initialized || !g_hooks.empty()) return;
    const MH_STATUS status = MH_Uninitialize();
    if (status == MH_OK || status == MH_ERROR_NOT_INITIALIZED) g_initialized = false;
}

BOOL RestoreCommittedStatesLocked() noexcept {
    if (!g_initialized) return true;
    bool queued = true;
    bool anyBackendEntries = false;
    for (const auto& entry : g_hooks) {
        if (entry.second.backendRemoved) continue;
        anyBackendEntries = true;
        const MH_STATUS status = entry.second.enabled
            ? MH_QueueEnableHook(entry.first)
            : MH_QueueDisableHook(entry.first);
        if (status != MH_OK) queued = false;
    }
    if (!queued) return false;
    return !anyBackendEntries || MH_ApplyQueued() == MH_OK;
}

BOOL ApplyLocked() noexcept {
    if (g_hooks.empty()) {
        TryUninitializeLocked();
        return TRUE;
    }

    std::vector<void*> removals;
    bool anyBackendOperations = false;
    try {
        removals.reserve(g_hooks.size());

        // Queue the complete batch first. MinHook suspends other threads once
        // while applying the batch, avoiding a series of independently patched
        // functions with each intermediate state visible to running threads.
        for (const auto& entry : g_hooks) {
            const HookRecord& hook = entry.second;
            if (hook.backendRemoved) {
                if (hook.removeQueued) removals.push_back(hook.target);
                continue;
            }
            const MH_STATUS status = hook.removeQueued
                ? MH_QueueDisableHook(hook.target)
                : MH_QueueEnableHook(hook.target);
            if (status != MH_OK) {
                if (!RestoreCommittedStatesLocked()) {
                    OutputDebugStringW(L"00047: unable to restore queued hook operations.\n");
                }
                return FALSE;
            }
            anyBackendOperations = true;
            if (hook.removeQueued) removals.push_back(hook.target);
        }

        if (anyBackendOperations && MH_ApplyQueued() != MH_OK) {
            if (!RestoreCommittedStatesLocked()) {
                OutputDebugStringW(L"00047: hook batch failed and rollback was unsuccessful.\n");
            }
            return FALSE;
        }

        for (auto& entry : g_hooks) {
            if (!entry.second.backendRemoved) {
                entry.second.enabled = !entry.second.removeQueued;
            }
        }

        // First remove the backend detour; then release its shared cross-mod
        // claim. If either step fails, retain a record so shutdown can retry.
        bool removalFailed = false;
        for (void* target : removals) {
            auto it = g_hooks.find(target);
            if (it == g_hooks.end()) continue;
            HookRecord& hook = it->second;
            if (!hook.backendRemoved) {
                const MH_STATUS status = MH_RemoveHook(target);
                if (status != MH_OK && status != MH_ERROR_NOT_CREATED) {
                    removalFailed = true;
                    continue;
                }
                hook.backendRemoved = true;
                hook.enabled = false;
            }
            if (hook.registryClaimed) {
                if (!SharedHookRegistry().Release(target, hook.ownerModule)) {
                    removalFailed = true;
                    continue;
                }
                hook.registryClaimed = false;
            }
            if (hook.backendRemoved && !hook.registryClaimed) {
                g_hooks.erase(it);
            }
        }
        if (removalFailed) return FALSE;

        TryUninitializeLocked();
        return TRUE;
    } catch (...) {
        if (!RestoreCommittedStatesLocked()) {
            OutputDebugStringW(L"00047: exception while preparing hook batch; rollback failed.\n");
        }
        return FALSE;
    }
}

struct ThreadBasicInformationCompat {
    LONG exitStatus;
    PVOID tebBaseAddress;
    HANDLE uniqueProcess;
    HANDLE uniqueThread;
    ULONG_PTR affinityMask;
    LONG priority;
    LONG basePriority;
};

using NtQueryInformationThreadCompat = LONG (NTAPI*)(
    HANDLE, ULONG, PVOID, ULONG, PULONG);

bool IsModuleAddress(std::uintptr_t address,
    std::uintptr_t moduleStart, std::uintptr_t moduleEnd) noexcept {
    return address >= moduleStart && address < moduleEnd;
}

bool StackReferencesModule(HANDLE thread, const CONTEXT& context,
    std::uintptr_t moduleStart, std::uintptr_t moduleEnd,
    NtQueryInformationThreadCompat queryThread) noexcept {
    if (IsModuleAddress(static_cast<std::uintptr_t>(context.Rip),
            moduleStart, moduleEnd)) {
        return true;
    }

    // A mod-owned worker can be blocked inside kernel32/ntdll while its
    // instruction pointer and current stack no longer visibly reference the
    // mod. Query the original Win32 thread entry point as an additional guard.
    // ThreadQuerySetWin32StartAddress is information class 9 in ntdll.
    PVOID win32StartAddress = nullptr;
    constexpr ULONG kThreadQuerySetWin32StartAddress = 9;
    if (!queryThread ||
        queryThread(thread, kThreadQuerySetWin32StartAddress,
            &win32StartAddress, sizeof(win32StartAddress), nullptr) < 0) {
        return true; // Fail closed if a thread's module entry point is unknown.
    }
    if (IsModuleAddress(reinterpret_cast<std::uintptr_t>(win32StartAddress),
            moduleStart, moduleEnd)) {
        return true;
    }

    ThreadBasicInformationCompat information{};
    if (!queryThread ||
        queryThread(thread, 0, &information, sizeof(information), nullptr) < 0 ||
        !information.tebBaseAddress) {
        return true; // Fail closed: this thread could not be inspected.
    }

    PVOID stackBase = nullptr;
    SIZE_T bytesRead = 0;
    const auto* tebBytes = static_cast<const BYTE*>(information.tebBaseAddress);
    // NT_TIB.StackBase is the second pointer in the x64 TEB.
    if (!ReadProcessMemory(GetCurrentProcess(), tebBytes + sizeof(PVOID),
            &stackBase, sizeof(stackBase), &bytesRead) || bytesRead != sizeof(stackBase)) {
        return true;
    }

    const auto stackPointer = static_cast<std::uintptr_t>(context.Rsp);
    const auto stackLimit = reinterpret_cast<std::uintptr_t>(stackBase);
    constexpr std::uintptr_t kMaxStackScanBytes = 8 * 1024 * 1024;
    constexpr SIZE_T kChunkBytes = 16 * 1024;
    if (stackPointer == 0 || stackLimit <= stackPointer ||
        stackLimit - stackPointer > kMaxStackScanBytes) {
        return true; // An unexpected stack shape is not safe to unload.
    }

    std::uintptr_t cursor = (stackPointer + sizeof(std::uintptr_t) - 1) &
        ~(static_cast<std::uintptr_t>(sizeof(std::uintptr_t)) - 1);
    std::array<BYTE, kChunkBytes> bytes{};
    while (cursor < stackLimit) {
        const SIZE_T requested = static_cast<SIZE_T>(
            std::min<std::uintptr_t>(kChunkBytes, stackLimit - cursor));
        SIZE_T actual = 0;
        if (!ReadProcessMemory(GetCurrentProcess(),
                reinterpret_cast<const void*>(cursor), bytes.data(),
                requested, &actual) || actual == 0) {
            return true; // Incomplete stack inspection is not proof of quiescence.
        }

        const SIZE_T alignedBytes = actual -
            (actual % sizeof(std::uintptr_t));
        for (SIZE_T offset = 0; offset < alignedBytes; offset += sizeof(std::uintptr_t)) {
            std::uintptr_t possibleReturnAddress = 0;
            std::memcpy(&possibleReturnAddress, bytes.data() + offset,
                sizeof(possibleReturnAddress));
            if (IsModuleAddress(possibleReturnAddress, moduleStart, moduleEnd)) {
                return true;
            }
        }
        cursor += actual;
        if (actual < requested) return true;
    }
    return false;
}

bool HasNoInFlightModuleFrames() noexcept {
    HMODULE module = nullptr;
    if (!GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS |
                GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            reinterpret_cast<LPCWSTR>(&Wh_SetFunctionHook), &module) ||
        !module) {
        return false;
    }

    MODULEINFO moduleInfo{};
    if (!GetModuleInformation(GetCurrentProcess(), module,
            &moduleInfo, sizeof(moduleInfo)) || !moduleInfo.lpBaseOfDll ||
        moduleInfo.SizeOfImage == 0) {
        return false;
    }
    const auto moduleStart = reinterpret_cast<std::uintptr_t>(moduleInfo.lpBaseOfDll);
    const auto moduleEnd = moduleStart + moduleInfo.SizeOfImage;
    if (moduleEnd < moduleStart) return false;

    HMODULE ntdll = GetModuleHandleW(L"ntdll.dll");
    auto queryThread = ntdll
        ? reinterpret_cast<NtQueryInformationThreadCompat>(
            GetProcAddress(ntdll, "NtQueryInformationThread"))
        : nullptr;
    if (!queryThread) return false;

    const DWORD currentPid = GetCurrentProcessId();
    const DWORD currentTid = GetCurrentThreadId();
    HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
    if (snapshot == INVALID_HANDLE_VALUE) return false;

    struct SuspendedThread {
        HANDLE handle = nullptr;
        DWORD previousSuspendCount = 0;
    };
    std::vector<SuspendedThread> suspended;
    auto resumeAll = [&]() noexcept {
        for (auto it = suspended.rbegin(); it != suspended.rend(); ++it) {
            if (it->handle) {
                // Resume exactly the suspension added here; preserve any
                // suspension count that existed before this probe.
                ResumeThread(it->handle);
                CloseHandle(it->handle);
            }
        }
        suspended.clear();
    };

    bool safe = true;
    try {
        suspended.reserve(256);
        THREADENTRY32 entry{};
        entry.dwSize = sizeof(entry);
        if (!Thread32First(snapshot, &entry)) {
            CloseHandle(snapshot);
            return false;
        }

        do {
            if (entry.th32OwnerProcessID != currentPid ||
                entry.th32ThreadID == currentTid) {
                continue;
            }

            HANDLE thread = OpenThread(
                THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT |
                    THREAD_QUERY_INFORMATION,
                FALSE, entry.th32ThreadID);
            if (!thread) {
                // The thread may have exited between the snapshot and OpenThread.
                if (GetLastError() == ERROR_INVALID_PARAMETER) continue;
                safe = false;
                break;
            }

            const DWORD previous = SuspendThread(thread);
            if (previous == static_cast<DWORD>(-1)) {
                CloseHandle(thread);
                safe = false;
                break;
            }
            try {
                suspended.push_back({thread, previous});
            } catch (...) {
                ResumeThread(thread);
                CloseHandle(thread);
                safe = false;
                break;
            }
        } while (Thread32Next(snapshot, &entry));
        if (safe && GetLastError() != ERROR_NO_MORE_FILES) safe = false;

        CloseHandle(snapshot);
        snapshot = INVALID_HANDLE_VALUE;

        if (safe) {
            for (const auto& item : suspended) {
                CONTEXT context{};
                context.ContextFlags = CONTEXT_CONTROL;
                if (!GetThreadContext(item.handle, &context)) {
                    safe = false;
                    break;
                }
                if (StackReferencesModule(item.handle, context,
                        moduleStart, moduleEnd, queryThread)) {
                    safe = false;
                    break;
                }
            }
        }
    } catch (...) {
        safe = false;
    }

    if (snapshot != INVALID_HANDLE_VALUE) CloseHandle(snapshot);
    resumeAll();
    return safe;
}


} // namespace

BOOL Wh_SetFunctionHook(void* targetFunction, void* hookFunction, void** originalFunction) {
    if (originalFunction) *originalFunction = nullptr;
    if (!targetFunction || !hookFunction || targetFunction == hookFunction) return FALSE;
    std::lock_guard<std::mutex> lock(g_mutex);
    if (!g_accepting || g_hooks.find(targetFunction) != g_hooks.end()) return FALSE;

    HMODULE ownerModule = CurrentHookOwnerModule();
    if (!ownerModule || !SharedHookRegistry().Claim(targetFunction, ownerModule)) {
        return FALSE;
    }
    if (!EnsureInitializedLocked()) {
        (void)SharedHookRegistry().Release(targetFunction, ownerModule);
        return FALSE;
    }

    void* trampoline = nullptr;
    const MH_STATUS status = MH_CreateHook(targetFunction, hookFunction, &trampoline);
    if (status != MH_OK) {
        (void)SharedHookRegistry().Release(targetFunction, ownerModule);
        TryUninitializeLocked();
        return FALSE;
    }

    try {
        const auto inserted = g_hooks.emplace(targetFunction, HookRecord{
            targetFunction, hookFunction, trampoline, false, false,
            ownerModule, true, false
        });
        if (!inserted.second) {
            const MH_STATUS removed = MH_RemoveHook(targetFunction);
            if (removed == MH_OK || removed == MH_ERROR_NOT_CREATED) {
                (void)SharedHookRegistry().Release(targetFunction, ownerModule);
                TryUninitializeLocked();
            }
            return FALSE;
        }
    } catch (...) {
        // If MinHook cleanup or shared-claim release fails, the shared registry
        // retains the claim and CanUnload() refuses to unmap this module.
        const MH_STATUS removed = MH_RemoveHook(targetFunction);
        if (removed == MH_OK || removed == MH_ERROR_NOT_CREATED) {
            (void)SharedHookRegistry().Release(targetFunction, ownerModule);
        }
        TryUninitializeLocked();
        return FALSE;
    }
    if (originalFunction) *originalFunction = trampoline;
    return TRUE;
}

BOOL Wh_RemoveFunctionHook(void* targetFunction) {
    if (!targetFunction) return FALSE;
    std::lock_guard<std::mutex> lock(g_mutex);
    if (!g_accepting) return FALSE;
    auto it = g_hooks.find(targetFunction);
    if (it == g_hooks.end() || it->second.removeQueued) return FALSE;
    it->second.removeQueued = true;
    return TRUE;
}

BOOL Wh_ApplyHookOperations(void) {
    std::lock_guard<std::mutex> lock(g_mutex);
    if (!g_accepting && !g_hooks.empty()) return FALSE;
    return ApplyLocked();
}

namespace wmod::windhawk_hook_compat {
BOOL BeginHookOperations() noexcept {
    std::lock_guard<std::mutex> lock(g_mutex);
    if (!g_hooks.empty()) return FALSE;
    g_accepting = true;
    return TRUE;
}


BOOL CanUnload() noexcept {
    try {
        {
            std::lock_guard<std::mutex> lock(g_mutex);
            // The lifecycle adapter must have stopped accepting hooks, applied
            // all removals, and left no MinHook entries registered.
            if (g_accepting || !g_hooks.empty() || g_initialized) return FALSE;
        }
        HMODULE module = CurrentHookOwnerModule();
        if (!module || SharedHookRegistry().ReferencesModule(module)) return FALSE;
        return HasNoInFlightModuleFrames() ? TRUE : FALSE;
    } catch (...) {
        return FALSE;
    }
}

BOOL ApplyHookOperations() noexcept {
    try {
        std::lock_guard<std::mutex> lock(g_mutex);
        return ApplyLocked();
    } catch (...) {
        return FALSE;
    }
}

void ShutdownHookOperations() noexcept {
    std::lock_guard<std::mutex> lock(g_mutex);
    for (auto& entry : g_hooks) entry.second.removeQueued = true;
    g_accepting = false;
    if (!ApplyLocked()) {
        // Best-effort retry, but never discard bookkeeping if Windows refuses
        // to remove a hook. CanUnload() stays false and the host retains the DLL.
        for (auto it = g_hooks.begin(); it != g_hooks.end();) {
            HookRecord& hook = it->second;
            if (!hook.backendRemoved && hook.enabled) {
                if (MH_DisableHook(it->first) != MH_OK) {
                    ++it;
                    continue;
                }
                hook.enabled = false;
            }
            if (!hook.backendRemoved) {
                const MH_STATUS status = MH_RemoveHook(it->first);
                if (status != MH_OK && status != MH_ERROR_NOT_CREATED) {
                    ++it;
                    continue;
                }
                hook.backendRemoved = true;
                hook.enabled = false;
            }
            if (hook.registryClaimed) {
                if (!SharedHookRegistry().Release(it->first, hook.ownerModule)) {
                    ++it;
                    continue;
                }
                hook.registryClaimed = false;
            }
            it = g_hooks.erase(it);
        }
        TryUninitializeLocked();
        if (!g_hooks.empty() || g_initialized) {
            OutputDebugStringW(L"00047: hook shutdown could not prove safe removal; DLL must remain loaded.\n");
        }
    }
}
} // namespace wmod::windhawk_hook_compat

BOOL Wh_Disasm(void* address, WH_DISASM_RESULT* result) {
    if (!result) return FALSE;
    std::memset(result, 0, sizeof(*result));
    if (!address) return FALSE;

    try {
        MEMORY_BASIC_INFORMATION region{};
        if (VirtualQuery(address, &region, sizeof(region)) != sizeof(region) ||
            region.State != MEM_COMMIT ||
            (region.Protect & (PAGE_GUARD | PAGE_NOACCESS))) {
            return FALSE;
        }
        const DWORD protection = region.Protect & 0xFF;
        if (protection == PAGE_EXECUTE) return FALSE;

        const auto startAddress = reinterpret_cast<std::uintptr_t>(address);
        const auto regionBase = reinterpret_cast<std::uintptr_t>(region.BaseAddress);
        if (startAddress < regionBase ||
            startAddress - regionBase >= region.RegionSize) return FALSE;
        const size_t available = std::min<size_t>(
            15, region.RegionSize - static_cast<size_t>(startAddress - regionBase));
        if (available == 0) return FALSE;

        unsigned char bytes[15]{};
        SIZE_T bytesRead = 0;
        if (!ReadProcessMemory(GetCurrentProcess(), address, bytes, available, &bytesRead) ||
            bytesRead == 0) {
            return FALSE;
        }

        // diStorm provides full x64 instruction decoding and Intel-syntax text.
        std::array<_DecodedInst, 15> decoded{};
        unsigned int decodedCount = 0;
        const _DecodeResult decodeStatus = distorm_decode64(
            static_cast<_OffsetType>(startAddress),
            bytes,
            static_cast<int>(bytesRead),
            Decode64Bits,
            decoded.data(),
            static_cast<unsigned int>(decoded.size()),
            &decodedCount);
        if ((decodeStatus != DECRES_SUCCESS && decodeStatus != DECRES_MEMORYERR) ||
            decodedCount == 0) {
            return FALSE;
        }

        const _DecodedInst& instruction = decoded[0];
        if (instruction.size == 0 || instruction.size > bytesRead ||
            instruction.mnemonic.length == 0) {
            return FALSE;
        }

        std::string text(
            reinterpret_cast<const char*>(instruction.mnemonic.p),
            instruction.mnemonic.length);
        if (instruction.operands.length != 0) {
            text.push_back(' ');
            text.append(reinterpret_cast<const char*>(instruction.operands.p),
                instruction.operands.length);
        }

        result->length = instruction.size;
        const size_t copyLength = std::min(text.size(), sizeof(result->text) - 1);
        std::memcpy(result->text, text.data(), copyLength);
        result->text[copyLength] = '\0';
        return TRUE;
    } catch (...) {
        std::memset(result, 0, sizeof(*result));
        return FALSE;
    }
}
