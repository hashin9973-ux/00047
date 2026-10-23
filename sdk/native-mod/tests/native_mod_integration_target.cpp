// CI-only test double for the remote native-mod loader. The executable is
// named explorer.exe to exercise the production target-name allow-list, but it
// never launches or modifies the real Windows shell.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <Windows.h>

int main() {
    wchar_t readyPath[32768]{};
    const DWORD length = GetEnvironmentVariableW(
        L"WMOD_NATIVE_TARGET_READY_FILE", readyPath, _countof(readyPath));
    if (length > 0 && length < _countof(readyPath)) {
        HANDLE marker = CreateFileW(
            readyPath, GENERIC_WRITE, FILE_SHARE_READ, nullptr,
            CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
        if (marker != INVALID_HANDLE_VALUE) CloseHandle(marker);
    }

    // Stay alive for remote load/unload tests. CI terminates this process after
    // every DLL lifecycle has completed.
    for (;;) Sleep(1000);
}
