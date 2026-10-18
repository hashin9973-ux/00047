#include <winsock2.h>
#include <ws2tcpip.h>
#include "windhawk_nonhook_compat.h"

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <iterator>
#include <string>
#include <string_view>
#include <thread>
#include <utility>
#include <vector>

namespace {
std::filesystem::path ExecutableDirectory() {
    wchar_t buffer[32768]{};
    const DWORD length = GetModuleFileNameW(nullptr, buffer, _countof(buffer));
    if (length == 0 || length >= _countof(buffer)) return {};
    return std::filesystem::path(std::wstring(buffer, length)).parent_path();
}

bool WriteUtf8(const std::filesystem::path& path, std::string_view contents) {
    HANDLE file = CreateFileW(path.c_str(), GENERIC_WRITE, 0, nullptr,
        CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (file == INVALID_HANDLE_VALUE) return false;
    size_t offset = 0;
    bool ok = true;
    while (offset < contents.size()) {
        DWORD written = 0;
        const DWORD requested = static_cast<DWORD>(
            (contents.size() - offset) > (1u << 20) ? (1u << 20) : (contents.size() - offset));
        if (!WriteFile(file, contents.data() + offset, requested, &written, nullptr) || written == 0) {
            ok = false;
            break;
        }
        offset += written;
    }
    CloseHandle(file);
    return ok && offset == contents.size();
}

bool Exists(const std::filesystem::path& path) {
    const DWORD attributes = GetFileAttributesW(path.c_str());
    return attributes != INVALID_FILE_ATTRIBUTES && (attributes & FILE_ATTRIBUTE_DIRECTORY) == 0;
}

bool StartLocalHttpResponse(std::string response, unsigned short& port, SOCKET& listener,
    std::thread& worker) {
    listener = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (listener == INVALID_SOCKET) return false;
    sockaddr_in address{};
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    address.sin_port = 0;
    if (bind(listener, reinterpret_cast<const sockaddr*>(&address), sizeof(address)) == SOCKET_ERROR ||
        listen(listener, 1) == SOCKET_ERROR) {
        closesocket(listener);
        listener = INVALID_SOCKET;
        return false;
    }
    int addressLength = sizeof(address);
    if (getsockname(listener, reinterpret_cast<sockaddr*>(&address), &addressLength) == SOCKET_ERROR) {
        closesocket(listener);
        listener = INVALID_SOCKET;
        return false;
    }
    port = ntohs(address.sin_port);
    worker = std::thread([listener, response = std::move(response)]() mutable {
        SOCKET client = accept(listener, nullptr, nullptr);
        if (client != INVALID_SOCKET) {
            char request[4096]{};
            (void)recv(client, request, sizeof(request), 0);
            size_t offset = 0;
            while (offset < response.size()) {
                const int written = send(client, response.data() + offset,
                    static_cast<int>(std::min<size_t>(response.size() - offset, 64 * 1024)), 0);
                if (written <= 0) break;
                offset += static_cast<size_t>(written);
            }
            shutdown(client, SD_BOTH);
            closesocket(client);
        }
        closesocket(listener);
    });
    return true;
}
}

#define CHECK(condition) do { \
    if (!(condition)) { \
        std::wcerr << L"FAILED check at line " << __LINE__ << L"\n"; \
        return 1; \
    } \
} while (false)

int wmain() {
    WSADATA wsaData{};
    CHECK(WSAStartup(MAKEWORD(2, 2), &wsaData) == 0);
    const auto directory = ExecutableDirectory();
    CHECK(!directory.empty());

    const auto manifest = directory / L"manifest.json";
    const auto settings = directory / L"settings.json";
    const auto values = directory / L"wh_values.ini";
    const auto log = directory / L"mod.log";
    DeleteFileW(settings.c_str());
    DeleteFileW(values.c_str());
    DeleteFileW(log.c_str());

    const std::string manifestJson = R"json({
      "manifestVersion": 1,
      "id": "com.example.compat-test",
      "name": "Compatibility Test",
      "version": "1.0.0",
      "architecture": "x64",
      "module": "bin/compat_test.dll",
      "targets": ["explorer.exe"],
      "settings": [
        {"id":"intSetting","label":"Integer","type":"integer","default":42},
        {"id":"boolSetting","label":"Boolean","type":"boolean","default":true},
        {"id":"stringSetting","label":"String","type":"string","default":"Hello \u263A"},
        {"id":"choice","label":"Choice","type":"select","default":"mica",
         "options":[{"value":"mica","label":"Mica"}]},
        {"id":"dynamicSetting","label":"Dynamic choice","type":"string","default":"missing",
         "dynamicSelect":true,"options":[{"value":"static","label":"Static"}]}
      ]
    })json";
    CHECK(WriteUtf8(manifest, manifestJson));

    // Defaults are read from manifest.json before the settings editor has saved
    // the first settings.json file.
    CHECK(Wh_GetIntSetting(L"intSetting") == 42);
    CHECK(Wh_GetIntSetting(L"boolSetting") == 1);
    CHECK(Wh_GetIntSetting(L"int%ls", L"Setting") == 42);

    PCWSTR defaultText = Wh_GetStringSetting(L"stringSetting");
    CHECK(defaultText != nullptr);
    CHECK(std::wcscmp(defaultText, L"Hello \u263A") == 0);
    Wh_FreeStringSetting(defaultText);

    PCWSTR defaultChoice = Wh_GetStringSetting(L"choice");
    CHECK(defaultChoice != nullptr);
    CHECK(std::wcscmp(defaultChoice, L"mica") == 0);
    Wh_FreeStringSetting(defaultChoice);

    PCWSTR missingText = Wh_GetStringSetting(L"missing");
    CHECK(missingText != nullptr);
    CHECK(missingText[0] == L'\0');
    Wh_FreeStringSetting(missingText);

    // settings.json overrides only keys present in the saved values; other keys
    // continue to resolve against their declared manifest defaults.
    CHECK(WriteUtf8(settings, R"json({"intSetting":7})json"));
    CHECK(Wh_GetIntSetting(L"intSetting") == 7);
    CHECK(Wh_GetIntSetting(L"boolSetting") == 1);
    CHECK(WriteUtf8(settings, R"json({"intSetting":7,"stringSetting":"Saved value"})json"));
    PCWSTR savedText = Wh_GetStringSetting(L"stringSetting");
    CHECK(savedText != nullptr);
    CHECK(std::wcscmp(savedText, L"Saved value") == 0);
    Wh_FreeStringSetting(savedText);

    // Invalid persisted types are ignored in favor of declared defaults.
    CHECK(WriteUtf8(settings, R"json({"intSetting":"invalid","stringSetting":false})json"));
    CHECK(Wh_GetIntSetting(L"intSetting") == 42);
    PCWSTR fallbackText = Wh_GetStringSetting(L"stringSetting");
    CHECK(fallbackText != nullptr);
    CHECK(std::wcscmp(fallbackText, L"Hello \u263A") == 0);
    Wh_FreeStringSetting(fallbackText);

    // Persistent simple values use the package-scoped INI storage file.
    CHECK(Wh_GetIntValue(L"counter", 17) == 17);
    CHECK(Wh_SetIntValue(L"counter", 19) == TRUE);
    CHECK(Wh_GetIntValue(L"counter", 17) == 19);
    CHECK(Wh_SetStringValue(L"greeting", L"persisted") == TRUE);
    wchar_t textBuffer[64]{};
    CHECK(Wh_GetStringValue(L"greeting", textBuffer, _countof(textBuffer)) == 9);
    CHECK(std::wcscmp(textBuffer, L"persisted") == 0);
    wchar_t tinyBuffer[2]{L'x', L'x'};
    CHECK(Wh_GetStringValue(L"greeting", tinyBuffer, _countof(tinyBuffer)) == 0);
    CHECK(tinyBuffer[0] == L'\0');
    CHECK(Wh_SetStringValue(L"invalid\r\nkey", L"blocked") == FALSE);

    // Runtime dynamic-select options use the documented value-storage namespace.
    CHECK(Wh_ClearDynamicSelectOptions(L"dynamicSetting") == TRUE);
    CHECK(Wh_SetDynamicSelectOption(L"dynamicSetting", L"speaker-id", L"Speakers") == TRUE);
    wchar_t dynamicLabel[128]{};
    constexpr PCWSTR kDynamicKey = L"::wh_select_option::dynamicSetting::speaker-id";
    CHECK(Wh_GetStringValue(kDynamicKey, dynamicLabel, _countof(dynamicLabel)) == 8);
    CHECK(std::wcscmp(dynamicLabel, L"Speakers") == 0);
    CHECK(Wh_SetDynamicSelectOption(L"dynamicSetting", L"static", L"Runtime static label") == TRUE);
    CHECK(Wh_GetStringValue(L"::wh_select_option::dynamicSetting::static",
        dynamicLabel, _countof(dynamicLabel)) == 20);
    CHECK(std::wcscmp(dynamicLabel, L"Runtime static label") == 0);
    // Source-level fallback stores options by key; the settings UI ignores
    // them unless the manifest actually declares dynamicSelect for that field.
    CHECK(Wh_SetDynamicSelectOption(L"choice", L"ignored", L"No dynamic flag") == TRUE);
    CHECK(Wh_DeleteDynamicSelectOption(L"choice", L"ignored") == TRUE);
    CHECK(Wh_SetDynamicSelectOption(L"dynamicSetting", L"bad", L"bad\r\nlabel") == FALSE);
    CHECK(Wh_SetDynamicSelectOption(L"dynamicSetting", L"", L"No device") == TRUE);
    CHECK(Wh_GetStringValue(L"::wh_select_option::dynamicSetting::", dynamicLabel,
        _countof(dynamicLabel)) == 9);
    CHECK(std::wcscmp(dynamicLabel, L"No device") == 0);
    CHECK(Wh_DeleteDynamicSelectOption(L"dynamicSetting", L"not-present") == TRUE);
    CHECK(Wh_DeleteDynamicSelectOption(L"dynamicSetting", L"") == TRUE);
    CHECK(Wh_DeleteDynamicSelectOption(L"dynamicSetting", L"speaker-id") == TRUE);
    CHECK(Wh_GetStringValue(kDynamicKey, dynamicLabel, _countof(dynamicLabel)) == 0);
    CHECK(Wh_SetDynamicSelectOption(L"dynamicSetting", L"speaker-id", L"Speakers") == TRUE);
    CHECK(Wh_ClearDynamicSelectOptions(L"dynamicSetting") == TRUE);
    CHECK(Wh_GetStringValue(kDynamicKey, dynamicLabel, _countof(dynamicLabel)) == 0);
    CHECK(Wh_GetStringValue(L"::wh_select_option::dynamicSetting::static",
        dynamicLabel, _countof(dynamicLabel)) == 0);

    // Binary values preserve arbitrary bytes and do not partially copy when
    // caller capacity is insufficient.
    const BYTE binaryInput[] = {0x00, 0x01, 0x7F, 0x80, 0xFF};
    CHECK(Wh_SetBinaryValue(L"binaryBlob", binaryInput, _countof(binaryInput)) == TRUE);
    BYTE binaryOutput[_countof(binaryInput)]{};
    CHECK(Wh_GetBinaryValue(L"binaryBlob", binaryOutput, _countof(binaryOutput)) == _countof(binaryInput));
    for (size_t i = 0; i < _countof(binaryInput); ++i) CHECK(binaryOutput[i] == binaryInput[i]);
    BYTE tooSmall[] = {0xAA, 0xBB};
    CHECK(Wh_GetBinaryValue(L"binaryBlob", tooSmall, _countof(tooSmall)) == 0);
    CHECK(tooSmall[0] == 0xAA && tooSmall[1] == 0xBB);
    CHECK(Wh_SetBinaryValue(L"invalid\r\nkey", binaryInput, _countof(binaryInput)) == FALSE);
    std::vector<BYTE> oversized(wmod::windhawk_compat::kMaxStorageBinaryBytes + 1, 0x5A);
    CHECK(Wh_SetBinaryValue(L"tooLarge", oversized.data(), oversized.size()) == FALSE);

    std::vector<BYTE> maximumInput(wmod::windhawk_compat::kMaxStorageBinaryBytes);
    for (size_t i = 0; i < maximumInput.size(); ++i)
        maximumInput[i] = static_cast<BYTE>((i * 37) & 0xFF);
    std::vector<BYTE> maximumOutput(maximumInput.size(), 0);
    CHECK(Wh_SetBinaryValue(L"maxBlob", maximumInput.data(), maximumInput.size()) == TRUE);
    CHECK(Wh_GetBinaryValue(L"maxBlob", maximumOutput.data(), maximumOutput.size()) == maximumOutput.size());
    for (size_t i = 0; i < maximumInput.size(); ++i) CHECK(maximumOutput[i] == maximumInput[i]);

    // Delete removes keys from either storage section.
    CHECK(Wh_DeleteValue(L"counter") == TRUE);
    CHECK(Wh_GetIntValue(L"counter", 17) == 17);
    CHECK(Wh_DeleteValue(L"binaryBlob") == TRUE);
    CHECK(Wh_GetBinaryValue(L"binaryBlob", binaryOutput, _countof(binaryOutput)) == 0);

    // Storage path API returns a usable package-scoped directory and fails
    // cleanly when the caller cannot fit the path plus terminator.
    std::vector<wchar_t> storageBuffer(32768, L'\0');
    const size_t storageLength = Wh_GetModStoragePath(storageBuffer.data(), storageBuffer.size());
    CHECK(storageLength > 0);
    CHECK(storageBuffer[storageLength] == L'\0');
    CHECK(std::filesystem::is_directory(std::filesystem::path(storageBuffer.data())));
    wchar_t tinyPath[2]{L'x', L'x'};
    CHECK(Wh_GetModStoragePath(tinyPath, _countof(tinyPath)) == 0);
    CHECK(tinyPath[0] == L'\0');

    // URL content retrieval is bounded, HTTP(S)-only and can return bytes or
    // atomically write the response body to a caller-provided file.
    CHECK(Wh_GetUrlContent(L"file:///C:/Windows/win.ini", nullptr) == nullptr);
    WH_GET_URL_CONTENT_OPTIONS invalidOptions{};
    CHECK(Wh_GetUrlContent(L"https://example.invalid/", &invalidOptions) == nullptr);

    unsigned short httpPort = 0;
    SOCKET httpListener = INVALID_SOCKET;
    std::thread httpWorker;
    CHECK(StartLocalHttpResponse(
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\nContent-Type: text/plain\r\n\r\nhello",
        httpPort, httpListener, httpWorker));
    const std::wstring httpUrl = L"http://127.0.0.1:" + std::to_wstring(httpPort) + L"/test?x=1";
    const WH_URL_CONTENT* memoryResponse = Wh_GetUrlContent(httpUrl.c_str(), nullptr);
    httpWorker.join();
    CHECK(memoryResponse != nullptr);
    CHECK(memoryResponse->statusCode == 200);
    CHECK(memoryResponse->length == 5);
    CHECK(memoryResponse->data != nullptr);
    CHECK(std::string(memoryResponse->data, memoryResponse->length) == "hello");
    Wh_FreeUrlContent(memoryResponse);

    unsigned short filePort = 0;
    SOCKET fileListener = INVALID_SOCKET;
    std::thread fileWorker;
    CHECK(StartLocalHttpResponse(
        "HTTP/1.1 201 Created\r\nContent-Length: 9\r\nConnection: close\r\nContent-Type: text/plain\r\n\r\nfile body",
        filePort, fileListener, fileWorker));
    const std::wstring fileUrl = L"http://127.0.0.1:" + std::to_wstring(filePort) + L"/file";
    const auto downloadedPath = directory / L"00047-url-content-test.txt";
    WH_GET_URL_CONTENT_OPTIONS fileOptions{sizeof(WH_GET_URL_CONTENT_OPTIONS), downloadedPath.c_str()};
    const WH_URL_CONTENT* fileResponse = Wh_GetUrlContent(fileUrl.c_str(), &fileOptions);
    fileWorker.join();
    CHECK(fileResponse != nullptr);
    CHECK(fileResponse->statusCode == 201);
    CHECK(fileResponse->length == 9);
    CHECK(fileResponse->data == nullptr);
    CHECK(Exists(downloadedPath));
    std::ifstream downloaded(downloadedPath, std::ios::binary);
    const std::string downloadedBody((std::istreambuf_iterator<char>(downloaded)),
        std::istreambuf_iterator<char>());
    CHECK(downloadedBody == "file body");
    downloaded.close();
    Wh_FreeUrlContent(fileResponse);
    DeleteFileW(downloadedPath.c_str());

    Wh_Log(L"non-hook compatibility smoke test: %d", 123);
    CHECK(Exists(log));

    DeleteFileW(settings.c_str());
    DeleteFileW(values.c_str());
    DeleteFileW(log.c_str());
    DeleteFileW(manifest.c_str());
    std::error_code cleanupError;
    std::filesystem::remove_all(directory / L"storage", cleanupError);
    CHECK(!cleanupError);
    WSACleanup();
    std::wcout << L"Non-hook compatibility tests passed.\n";
    return 0;
}
