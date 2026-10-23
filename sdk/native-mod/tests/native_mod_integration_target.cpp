// CI-only test double for the remote native-mod loader. The executable is
// named explorer.exe to exercise the production target-name allow-list, but it
// never launches or modifies the real Windows shell.
#include <Windows.h>

#include <cstdlib>
#include <string>

extern "C" __declspec(dllexport) __declspec(noinline)
int WINAPI RemoteTargetFunction(void) {
    volatile int value = 7;
    return value;
}

static void WriteProbeResponse(
    const std::wstring& responsePath,
    const std::wstring& temporaryPath,
    unsigned long long requestId,
    int result) {
    const std::string response =
        std::to_string(requestId) + " " + std::to_string(result) + "\n";
    HANDLE file = CreateFileW(temporaryPath.c_str(), GENERIC_WRITE, 0, nullptr,
        CREATE_ALWAYS, FILE_ATTRIBUTE_TEMPORARY, nullptr);
    if (file == INVALID_HANDLE_VALUE) return;
    DWORD written = 0;
    const BOOL ok = WriteFile(file, response.data(),
        static_cast<DWORD>(response.size()), &written, nullptr);
    CloseHandle(file);
    if (ok && written == static_cast<DWORD>(response.size())) {
        (void)MoveFileExW(temporaryPath.c_str(), responsePath.c_str(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH);
    } else {
        (void)DeleteFileW(temporaryPath.c_str());
    }
}

int main() {
    wchar_t readyPath[32768]{};
    const DWORD length = GetEnvironmentVariableW(
        L"WMOD_NATIVE_TARGET_READY_FILE", readyPath, (sizeof(readyPath) / sizeof(readyPath[0])));
    if (length > 0 && length < (sizeof(readyPath) / sizeof(readyPath[0]))) {
        HANDLE marker = CreateFileW(
            readyPath, GENERIC_WRITE, FILE_SHARE_READ, nullptr,
            CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
        if (marker != INVALID_HANDLE_VALUE) CloseHandle(marker);
    }

    wchar_t rootPath[32768]{};
    const DWORD rootLength = GetEnvironmentVariableW(
        L"WMOD_NATIVE_TARGET_ROOT", rootPath,
        static_cast<DWORD>(sizeof(rootPath) / sizeof(rootPath[0])));
    if (rootLength == 0 || rootLength >= sizeof(rootPath) / sizeof(rootPath[0])) {
        return 2;
    }
    const std::wstring root(rootPath, rootLength);
    const std::wstring requestPath = root + L"\\probe.request";
    const std::wstring responsePath = root + L"\\probe.response";
    const std::wstring temporaryPath = root + L"\\probe.response.tmp";

    // Stay alive for remote load/unload tests. Probe requests call the exported
    // function in this process so the host can observe an actual inline detour.
    // CI terminates this process after the lifecycle has completed.
    for (;;) {
        const DWORD attributes = GetFileAttributesW(requestPath.c_str());
        if (attributes != INVALID_FILE_ATTRIBUTES &&
            (attributes & FILE_ATTRIBUTE_DIRECTORY) == 0) {
            HANDLE request = CreateFileW(requestPath.c_str(), GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
            if (request != INVALID_HANDLE_VALUE) {
                char data[64]{};
                DWORD read = 0;
                const BOOL ok = ReadFile(request, data,
                    static_cast<DWORD>(sizeof(data) - 1), &read, nullptr);
                CloseHandle(request);
                (void)DeleteFileW(requestPath.c_str());
                if (ok && read > 0) {
                    char* end = nullptr;
                    const unsigned long long id = std::strtoull(data, &end, 10);
                    if (end != data && end &&
                        (*end == '\0' || *end == '\r' || *end == '\n' || *end == ' ')) {
                        WriteProbeResponse(responsePath, temporaryPath, id,
                            RemoteTargetFunction());
                    }
                }
            }
        }
        Sleep(5);
    }
}
