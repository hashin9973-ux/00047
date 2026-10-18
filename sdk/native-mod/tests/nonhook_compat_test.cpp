#include "windhawk_nonhook_compat.h"

#include <filesystem>
#include <iostream>
#include <string>
#include <string_view>

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
}

#define CHECK(condition) do { \
    if (!(condition)) { \
        std::wcerr << L"FAILED at line " << __LINE__ << L": " #condition << L"\n"; \
        return 1; \
    } \
} while (false)

int wmain() {
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
        {"id":"stringSetting","label":"String","type":"string","default":"Hello \\u263A"},
        {"id":"choice","label":"Choice","type":"select","default":"mica",
         "options":[{"value":"mica","label":"Mica"}]}
      ]
    })json";
    CHECK(WriteUtf8(manifest, manifestJson));

    // Defaults are read from manifest.json before the settings editor has saved
    // the first settings.json file.
    CHECK(Wh_GetIntSetting(L"intSetting") == 42);
    CHECK(Wh_GetIntSetting(L"boolSetting") == 1);
    CHECK(Wh_GetIntSetting(L"int%s", L"Setting") == 42);

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

    Wh_Log(L"non-hook compatibility smoke test: %d", 123);
    CHECK(Exists(log));

    DeleteFileW(settings.c_str());
    DeleteFileW(values.c_str());
    DeleteFileW(log.c_str());
    DeleteFileW(manifest.c_str());
    std::wcout << L"Non-hook compatibility tests passed.\n";
    return 0;
}
