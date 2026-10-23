#pragma once
#ifndef WMOD_00047_WINDHAWK_SETTINGS_CHANGED_ADAPTER_H
#define WMOD_00047_WINDHAWK_SETTINGS_CHANGED_ADAPTER_H

// Opt-in settings-change callback bridge for Windhawk source ports.
//
// The host stores settings.json beside the mod package. This adapter watches
// that directory using asynchronous ReadDirectoryChangesW and invokes the mod's
// Wh_ModSettingsChanged callback (void or BOOL(BOOL* bReload)) on a dedicated
// worker thread. Reload requests are surfaced through WmodIsReloadRequestedV1;
// the worker is synchronously stopped before shutdown callbacks run.
//
// Callback thread affinity differs from Windhawk. The callback must be thread
// safe, short-lived, and must not depend on a UI/shell thread. In particular,
// do not use this adapter for callbacks that mutate UI objects without proper
// marshaling. It does not make the mod sandboxed.
#ifndef WMOD_BUILD_DLL
#error Define WMOD_BUILD_DLL when building a native-mod DLL before including this header.
#endif

#include "00047_mod_services.h"
#include "windhawk_nonhook_adapter.h"

#include <Windows.h>
#include <array>
#include <atomic>
#include <cwchar>
#include <mutex>
#include <string>

namespace wmod::windhawk_settings_watch {
using Callback = void (*)();
using ReloadCallback = BOOL (*)(BOOL*);

constexpr DWORD kWatchFilter =
    FILE_NOTIFY_CHANGE_LAST_WRITE |
    FILE_NOTIFY_CHANGE_SIZE |
    FILE_NOTIFY_CHANGE_FILE_NAME |
    FILE_NOTIFY_CHANGE_ATTRIBUTES;
constexpr DWORD kWatchBufferBytes = 64 * 1024;

inline std::atomic<bool>& ReloadRequestFlag() noexcept {
    static std::atomic<bool> requested{false};
    return requested;
}

inline BOOL IsReloadRequested() noexcept {
    return ReloadRequestFlag().load(std::memory_order_acquire) ? TRUE : FALSE;
}

inline void ClearReloadRequest() noexcept {
    ReloadRequestFlag().store(false, std::memory_order_release);
}

class Watcher final {
public:
    Watcher() = default;
    Watcher(const Watcher&) = delete;
    Watcher& operator=(const Watcher&) = delete;

    ~Watcher() { (void)Stop(INFINITE); }

    bool Start(Callback callback) noexcept { return StartImpl(callback, nullptr); }

    bool Start(ReloadCallback callback) noexcept { return StartImpl(nullptr, callback); }

    // Returns false if mod callback code does not return promptly. In that
    // case keep every handle and callback pointer alive so the host can retain
    // the DLL and retry shutdown later instead of hanging the target process
    // or unloading code still executing on the worker.
    bool Stop(DWORD timeoutMs = 250) noexcept {
        std::lock_guard<std::mutex> guard(mutex_);
        if (!worker_) return true;

        if (stopEvent_) (void)SetEvent(stopEvent_);
        if (directory_ != INVALID_HANDLE_VALUE) (void)CancelIoEx(directory_, nullptr);
        const DWORD wait = WaitForSingleObject(worker_, timeoutMs);
        if (wait != WAIT_OBJECT_0) return false;

        CloseHandle(worker_);
        CloseHandle(readyEvent_);
        CloseHandle(stopEvent_);
        CloseHandle(directory_);
        worker_ = nullptr;
        readyEvent_ = nullptr;
        stopEvent_ = nullptr;
        directory_ = INVALID_HANDLE_VALUE;
        callback_ = nullptr;
        reloadCallback_ = nullptr;
        return true;
    }

private:
    bool StartImpl(Callback callback, ReloadCallback reloadCallback) noexcept {
        try {
            if (!callback && !reloadCallback) return false;
            std::lock_guard<std::mutex> guard(mutex_);
            if (worker_) return callback_ == callback && reloadCallback_ == reloadCallback;

            const auto package = wmod::PackageDirectory();
            if (package.empty()) return false;

            HANDLE directory = CreateFileW(
                package.c_str(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                nullptr,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                nullptr);
            if (directory == INVALID_HANDLE_VALUE) return false;

            HANDLE stop = CreateEventW(nullptr, TRUE, FALSE, nullptr);
            HANDLE ready = CreateEventW(nullptr, TRUE, FALSE, nullptr);
            if (!stop || !ready) {
                if (stop) CloseHandle(stop);
                if (ready) CloseHandle(ready);
                CloseHandle(directory);
                return false;
            }

            directory_ = directory;
            stopEvent_ = stop;
            readyEvent_ = ready;
            callback_ = callback;
            reloadCallback_ = reloadCallback;
            worker_ = CreateThread(nullptr, 0, &Watcher::ThreadMain, this, 0, nullptr);
            if (!worker_) {
                CloseHandle(readyEvent_);
                CloseHandle(stopEvent_);
                CloseHandle(directory_);
                readyEvent_ = nullptr;
                stopEvent_ = nullptr;
                directory_ = INVALID_HANDLE_VALUE;
                callback_ = nullptr;
                reloadCallback_ = nullptr;
                return false;
            }
            // Do not report a successful start until the first directory watch
            // request is pending; this avoids losing an immediate settings write.
            if (WaitForSingleObject(readyEvent_, 5000) != WAIT_OBJECT_0) {
                SetEvent(stopEvent_);
                CancelIoEx(directory_, nullptr);
                WaitForSingleObject(worker_, INFINITE);
                CloseHandle(worker_);
                CloseHandle(readyEvent_);
                CloseHandle(stopEvent_);
                CloseHandle(directory_);
                worker_ = nullptr;
                readyEvent_ = nullptr;
                stopEvent_ = nullptr;
                directory_ = INVALID_HANDLE_VALUE;
                callback_ = nullptr;
                reloadCallback_ = nullptr;
                return false;
            }
            return true;
        } catch (...) {
            return false;
        }
    }


    static bool ContainsSettingsChange(
        const BYTE* bytes, DWORD byteCount) noexcept {
        try {
            if (!bytes || byteCount == 0) {
                // Windows reports a zero-byte completion if the notification
                // buffer overflowed. Reloading is safer than silently missing it.
                return true;
            }
            DWORD offset = 0;
            while (offset + offsetof(FILE_NOTIFY_INFORMATION, FileName) <= byteCount) {
                const auto* entry = reinterpret_cast<const FILE_NOTIFY_INFORMATION*>(
                    bytes + offset);
                const DWORD headerBytes = static_cast<DWORD>(
                    offsetof(FILE_NOTIFY_INFORMATION, FileName));
                if (entry->FileNameLength > byteCount - offset - headerBytes ||
                    (entry->FileNameLength % sizeof(wchar_t)) != 0) break;
                const std::wstring name(
                    entry->FileName,
                    entry->FileNameLength / sizeof(wchar_t));
                if (CompareStringOrdinal(
                        name.c_str(), static_cast<int>(name.size()),
                        L"settings.json", -1, TRUE) == CSTR_EQUAL) {
                    return true;
                }
                if (entry->NextEntryOffset == 0) break;
                if (entry->NextEntryOffset < headerBytes ||
                    entry->NextEntryOffset > byteCount - offset) break;
                offset += entry->NextEntryOffset;
            }
        } catch (...) {}
        return false;
    }

    static DWORD WINAPI ThreadMain(LPVOID parameter) noexcept {
        auto* self = static_cast<Watcher*>(parameter);
        if (!self) return 1;
        const HANDLE directory = self->directory_;
        const HANDLE stop = self->stopEvent_;
        const HANDLE ready = self->readyEvent_;
        bool signaledReady = false;

        alignas(FILE_NOTIFY_INFORMATION) std::array<BYTE, kWatchBufferBytes> buffer{};
        bool notifyPending = false;
        while (true) {
            OVERLAPPED overlapped{};
            overlapped.hEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
            if (!overlapped.hEvent) return 2;

            const BOOL submitted = ReadDirectoryChangesW(
                directory,
                buffer.data(),
                static_cast<DWORD>(buffer.size()),
                FALSE,
                kWatchFilter,
                nullptr,
                &overlapped,
                nullptr);
            const DWORD submitError = submitted ? ERROR_SUCCESS : GetLastError();
            if (!submitted && submitError != ERROR_IO_PENDING) {
                const DWORD error = submitError;
                CloseHandle(overlapped.hEvent);
                if (error == ERROR_NOTIFY_ENUM_DIR) {
                    notifyPending = true;
                    continue;
                }
                if (WaitForSingleObject(stop, 250) == WAIT_OBJECT_0) return 0;
                continue;
            }

            if (!signaledReady) {
                SetEvent(ready);
                signaledReady = true;
            }

            // Arm the next directory request before invoking mod code. That way
            // edits made while Wh_ModSettingsChanged runs are not lost because
            // this worker temporarily stopped listening for directory events.
            if (notifyPending) {
                notifyPending = false;
                if (WaitForSingleObject(stop, 0) != WAIT_OBJECT_0) {
                    const Callback callback = self->callback_;
                    const ReloadCallback reloadCallback = self->reloadCallback_;
                    if (reloadCallback) {
                        BOOL reload = FALSE;
                        BOOL succeeded = FALSE;
                        try { succeeded = reloadCallback(&reload); } catch (...) {}
                        if (succeeded && reload) {
                            ReloadRequestFlag().store(true, std::memory_order_release);
                        }
                    } else if (callback) {
                        try { callback(); } catch (...) {
                            // No C++ exception may escape a Windows thread entrypoint.
                        }
                    }
                }
            }

            HANDLE waitHandles[2] = {stop, overlapped.hEvent};
            const DWORD wait = WaitForMultipleObjects(2, waitHandles, FALSE, INFINITE);
            if (wait == WAIT_OBJECT_0) {
                CancelIoEx(directory, &overlapped);
                DWORD ignored = 0;
                (void)GetOverlappedResult(directory, &overlapped, &ignored, TRUE);
                CloseHandle(overlapped.hEvent);
                return 0;
            }
            if (wait != WAIT_OBJECT_0 + 1) {
                CancelIoEx(directory, &overlapped);
                DWORD ignored = 0;
                (void)GetOverlappedResult(directory, &overlapped, &ignored, TRUE);
                CloseHandle(overlapped.hEvent);
                return 3;
            }

            DWORD transferred = 0;
            const BOOL completed = GetOverlappedResult(
                directory, &overlapped, &transferred, FALSE);
            CloseHandle(overlapped.hEvent);
            if (!completed) {
                const DWORD error = GetLastError();
                if (error == ERROR_OPERATION_ABORTED &&
                    WaitForSingleObject(stop, 0) == WAIT_OBJECT_0) {
                    return 0;
                }
                if (error == ERROR_NOTIFY_ENUM_DIR) {
                    // Notification overflow means individual changes were lost.
                    notifyPending = true;
                }
                continue;
            }

            if (ContainsSettingsChange(buffer.data(), transferred)) {
                notifyPending = true;
            }
        }
    }

    std::mutex mutex_;
    HANDLE directory_ = INVALID_HANDLE_VALUE;
    HANDLE stopEvent_ = nullptr;
    HANDLE readyEvent_ = nullptr;
    HANDLE worker_ = nullptr;
    Callback callback_ = nullptr;
    ReloadCallback reloadCallback_ = nullptr;
};

inline Watcher& Instance() {
    static Watcher watcher;
    return watcher;
}
} // namespace wmod::windhawk_settings_watch

// Define this instead of WMOD_DEFINE_WINDHAWK_LIFECYCLE when a non-hook
// Windhawk source port also defines Wh_ModSettingsChanged(). That callback is
// invoked on the watcher thread when settings.json changes. The callback must
// not throw, block for long periods, or assume shell/UI thread affinity.
#define WMOD_DEFINE_WINDHAWK_LIFECYCLE_WITH_SETTINGS_CHANGED( \
    init_callback, after_init_callback, settings_changed_callback, \
    before_uninit_callback, uninit_callback) \
    WMOD_00047_EXPORT std::uint32_t WINAPI WmodGetApiVersion(void) { \
        return WMOD_00047_ABI_VERSION; \
    } \
    static BOOL WINAPI WmodTryFinalizeShutdownV1(void) { \
        auto& state = ::wmod::windhawk_adapter::LifecycleState(); \
        if (state.load(std::memory_order_acquire) != 3) return state.load() == 0 ? TRUE : FALSE; \
        static std::mutex finalizerMutex; \
        std::lock_guard<std::mutex> guard(finalizerMutex); \
        if (state.load(std::memory_order_acquire) != 3) return state.load() == 0 ? TRUE : FALSE; \
        if (!::wmod::windhawk_settings_watch::Instance().Stop()) return FALSE; \
        try { (before_uninit_callback)(); } catch (...) {} \
        ::wmod::windhawk_hook_compat::ShutdownHookOperations(); \
        try { (uninit_callback)(); } catch (...) {} \
        state.store(0, std::memory_order_release); \
        return TRUE; \
    } \
    WMOD_00047_EXPORT BOOL WINAPI WmodIsInitializedV1(void) { \
        return ::wmod::windhawk_adapter::LifecycleState().load() == 2 ? TRUE : FALSE; \
    } \
    WMOD_00047_EXPORT BOOL WINAPI WmodCanUnloadV1(void) { \
        auto& state = ::wmod::windhawk_adapter::LifecycleState(); \
        if (state.load(std::memory_order_acquire) == 3 && !WmodTryFinalizeShutdownV1()) return FALSE; \
        return ::wmod::windhawk_hook_compat::CanUnload(); \
    } \
    WMOD_00047_EXPORT BOOL WINAPI WmodIsReloadRequestedV1(void) { \
        return ::wmod::windhawk_settings_watch::IsReloadRequested(); \
    } \
    WMOD_00047_EXPORT BOOL WINAPI WmodInitializeV1(void) { \
        int expected = 0; \
        auto& state = ::wmod::windhawk_adapter::LifecycleState(); \
        if (!state.compare_exchange_strong(expected, 1)) return expected == 2 ? TRUE : FALSE; \
        if (!::wmod::windhawk_hook_compat::BeginHookOperations()) { state.store(0); return FALSE; } \
        ::wmod::windhawk_settings_watch::ClearReloadRequest(); \
        bool init_succeeded = false; \
        try { \
            if (!(init_callback)()) { \
                ::wmod::windhawk_hook_compat::ShutdownHookOperations(); \
                state.store(0); \
                return FALSE; \
            } \
            init_succeeded = true; \
            if (!::wmod::windhawk_hook_compat::ApplyHookOperations()) { \
                try { (before_uninit_callback)(); } catch (...) {} \
                ::wmod::windhawk_hook_compat::ShutdownHookOperations(); \
                try { (uninit_callback)(); } catch (...) {} \
                state.store(0); \
                return FALSE; \
            } \
            (after_init_callback)(); \
            if (!::wmod::windhawk_settings_watch::Instance().Start(settings_changed_callback)) { \
                try { (before_uninit_callback)(); } catch (...) {} \
                ::wmod::windhawk_hook_compat::ShutdownHookOperations(); \
                try { (uninit_callback)(); } catch (...) {} \
                state.store(0); \
                return FALSE; \
            } \
            state.store(2); \
            return TRUE; \
        } catch (...) { \
            ::wmod::windhawk_settings_watch::Instance().Stop(); \
            if (init_succeeded) { \
                try { (before_uninit_callback)(); } catch (...) {} \
                ::wmod::windhawk_hook_compat::ShutdownHookOperations(); \
                try { (uninit_callback)(); } catch (...) {} \
            } else { \
                ::wmod::windhawk_hook_compat::ShutdownHookOperations(); \
            } \
            state.store(0); \
            return FALSE; \
        } \
    } \
    WMOD_00047_EXPORT void WINAPI WmodShutdownV1(void) { \
        int expected = 2; \
        auto& state = ::wmod::windhawk_adapter::LifecycleState(); \
        if (state.compare_exchange_strong(expected, 3)) { \
            (void)WmodTryFinalizeShutdownV1(); \
            return; \
        } \
        if (expected == 3) { \
            (void)WmodTryFinalizeShutdownV1(); \
            return; \
        } \
        if (expected == 0) ::wmod::windhawk_hook_compat::ShutdownHookOperations(); \
    }

#endif // WMOD_00047_WINDHAWK_SETTINGS_CHANGED_ADAPTER_H
