#pragma once
#ifndef WMOD_00047_WINDHAWK_SETTINGS_CHANGED_ADAPTER_H
#define WMOD_00047_WINDHAWK_SETTINGS_CHANGED_ADAPTER_H

// Opt-in settings-change callback bridge for Windhawk source ports.
//
// The host stores settings.json beside the mod package. This adapter watches
// that directory using asynchronous ReadDirectoryChangesW and invokes the mod's
// Wh_ModSettingsChanged callback on a dedicated worker thread. The worker is
// synchronously stopped before Wh_ModBeforeUninit/Wh_ModUninit run.
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
#include <cwchar>
#include <mutex>
#include <string>

namespace wmod::windhawk_settings_watch {
using Callback = void (*)();

constexpr DWORD kWatchFilter =
    FILE_NOTIFY_CHANGE_LAST_WRITE |
    FILE_NOTIFY_CHANGE_SIZE |
    FILE_NOTIFY_CHANGE_FILE_NAME |
    FILE_NOTIFY_CHANGE_ATTRIBUTES;
constexpr DWORD kWatchBufferBytes = 64 * 1024;

class Watcher final {
public:
    Watcher() = default;
    Watcher(const Watcher&) = delete;
    Watcher& operator=(const Watcher&) = delete;

    ~Watcher() { Stop(); }

    bool Start(Callback callback) noexcept {
        try {
            if (!callback) return false;
            std::lock_guard<std::mutex> guard(mutex_);
            if (worker_) return callback_ == callback;

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
            worker_ = CreateThread(nullptr, 0, &Watcher::ThreadMain, this, 0, nullptr);
            if (!worker_) {
                CloseHandle(readyEvent_);
                CloseHandle(stopEvent_);
                CloseHandle(directory_);
                readyEvent_ = nullptr;
                stopEvent_ = nullptr;
                directory_ = INVALID_HANDLE_VALUE;
                callback_ = nullptr;
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
                return false;
            }
            return true;
        } catch (...) {
            return false;
        }
    }

    void Stop() noexcept {
        std::lock_guard<std::mutex> guard(mutex_);
        if (!worker_) return;

        if (stopEvent_) SetEvent(stopEvent_);
        if (directory_ != INVALID_HANDLE_VALUE) CancelIoEx(directory_, nullptr);
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
    }

private:
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
            if (!submitted && GetLastError() != ERROR_IO_PENDING) {
                CloseHandle(overlapped.hEvent);
                if (WaitForSingleObject(stop, 250) == WAIT_OBJECT_0) return 0;
                continue;
            }

            if (!signaledReady) {
                SetEvent(ready);
                signaledReady = true;
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
                    // Prompt the mod to reread its settings from disk.
                    const Callback callback = self->callback_;
                    if (callback) {
                        try { callback(); } catch (...) {}
                    }
                }
                continue;
            }

            if (ContainsSettingsChange(buffer.data(), transferred)) {
                const Callback callback = self->callback_;
                if (callback) {
                    try {
                        callback();
                    } catch (...) {
                        // No C++ exception may escape a Windows thread entrypoint.
                    }
                }
            }
        }
    }

    std::mutex mutex_;
    HANDLE directory_ = INVALID_HANDLE_VALUE;
    HANDLE stopEvent_ = nullptr;
    HANDLE readyEvent_ = nullptr;
    HANDLE worker_ = nullptr;
    Callback callback_ = nullptr;
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
    WMOD_00047_EXPORT BOOL WINAPI WmodInitializeV1(void) { \
        int expected = 0; \
        auto& state = ::wmod::windhawk_adapter::LifecycleState(); \
        if (!state.compare_exchange_strong(expected, 1)) return expected == 2 ? TRUE : FALSE; \
        bool init_succeeded = false; \
        try { \
            if (!(init_callback)()) { state.store(0); return FALSE; } \
            init_succeeded = true; \
            (after_init_callback)(); \
            if (!::wmod::windhawk_settings_watch::Instance().Start(settings_changed_callback)) { \
                try { (before_uninit_callback)(); } catch (...) {} \
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
                try { (uninit_callback)(); } catch (...) {} \
            } \
            state.store(0); \
            return FALSE; \
        } \
    } \
    WMOD_00047_EXPORT void WINAPI WmodShutdownV1(void) { \
        int expected = 2; \
        auto& state = ::wmod::windhawk_adapter::LifecycleState(); \
        if (!state.compare_exchange_strong(expected, 3)) return; \
        ::wmod::windhawk_settings_watch::Instance().Stop(); \
        try { (before_uninit_callback)(); } catch (...) {} \
        try { (uninit_callback)(); } catch (...) {} \
        state.store(0); \
    }

#endif // WMOD_00047_WINDHAWK_SETTINGS_CHANGED_ADAPTER_H
