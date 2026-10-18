#pragma once
#ifndef WMOD_00047_WINDHAWK_NONHOOK_COMPAT_H
#define WMOD_00047_WINDHAWK_NONHOOK_COMPAT_H

// Source-level helpers for the subset of Windhawk APIs that do not perform
// function hooking or symbol discovery. This is deliberately not a Windhawk
// runtime replacement. In particular, this header does not declare or emulate
// Wh_SetFunctionHook, Wh_RemoveFunctionHook, Wh_ApplyHookOperations,
// Wh_FindFirstSymbol, or other hooking/symbol APIs.

#include "00047_mod_services.h"

#include <cstdarg>
#include <cstdint>
#include <cwchar>
#include <limits>
#include <string>
#include <string_view>
#include <vector>

namespace wmod::windhawk_compat {

inline constexpr size_t kMaxJsonBytes = 256 * 1024;
inline constexpr size_t kMaxSettingNameChars = 256;
inline constexpr size_t kMaxStorageStringChars = 32 * 1024;
inline constexpr size_t kMaxLogChars = 8192;

inline bool ReadUtf8File(const std::filesystem::path& path, std::wstring& output) noexcept {
    output.clear();
    try {
        HANDLE file = CreateFileW(path.c_str(), GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
        if (file == INVALID_HANDLE_VALUE) return false;

        LARGE_INTEGER size{};
        if (!GetFileSizeEx(file, &size) || size.QuadPart < 0 ||
            static_cast<unsigned long long>(size.QuadPart) > kMaxJsonBytes) {
            CloseHandle(file);
            return false;
        }

        std::string bytes(static_cast<size_t>(size.QuadPart), '\0');
        size_t offset = 0;
        bool ok = true;
        while (offset < bytes.size()) {
            DWORD read = 0;
            const DWORD requested = static_cast<DWORD>(
                (bytes.size() - offset) > (1u << 20) ? (1u << 20) : (bytes.size() - offset));
            if (!ReadFile(file, bytes.data() + offset, requested, &read, nullptr) || read == 0) {
                ok = false;
                break;
            }
            offset += read;
        }
        CloseHandle(file);
        if (!ok || offset != bytes.size()) return false;
        if (bytes.empty()) return true;

        const int count = MultiByteToWideChar(
            CP_UTF8, MB_ERR_INVALID_CHARS, bytes.data(), static_cast<int>(bytes.size()),
            nullptr, 0);
        if (count <= 0) return false;
        output.resize(static_cast<size_t>(count));
        if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, bytes.data(),
                static_cast<int>(bytes.size()), output.data(), count) != count) {
            output.clear();
            return false;
        }
        return true;
    } catch (...) {
        output.clear();
        return false;
    }
}

inline size_t SkipWhitespace(std::wstring_view json, size_t position) noexcept {
    while (position < json.size() &&
        (json[position] == L' ' || json[position] == L'\t' ||
         json[position] == L'\r' || json[position] == L'\n')) {
        ++position;
    }
    return position;
}

inline bool ParseStringToken(std::wstring_view json, size_t position,
    size_t& end, std::wstring* decoded = nullptr) {
    if (position >= json.size() || json[position] != L'"') return false;
    std::wstring local;
    size_t i = position + 1;
    for (; i < json.size(); ++i) {
        wchar_t ch = json[i];
        if (ch == L'"') {
            end = i + 1;
            if (decoded) *decoded = std::move(local);
            return true;
        }
        if (static_cast<unsigned int>(ch) < 0x20) return false;
        if (ch != L'\\') {
            if (decoded) local.push_back(ch);
            continue;
        }
        if (++i >= json.size()) return false;
        ch = json[i];
        switch (ch) {
            case L'"': if (decoded) local.push_back(L'"'); break;
            case L'\\': if (decoded) local.push_back(L'\\'); break;
            case L'/': if (decoded) local.push_back(L'/'); break;
            case L'b': if (decoded) local.push_back(L'\b'); break;
            case L'f': if (decoded) local.push_back(L'\f'); break;
            case L'n': if (decoded) local.push_back(L'\n'); break;
            case L'r': if (decoded) local.push_back(L'\r'); break;
            case L't': if (decoded) local.push_back(L'\t'); break;
            case L'u': {
                if (i + 4 >= json.size()) return false;
                unsigned int code = 0;
                for (size_t n = 1; n <= 4; ++n) {
                    const wchar_t digit = json[i + n];
                    unsigned int value = 0;
                    if (digit >= L'0' && digit <= L'9') value = static_cast<unsigned int>(digit - L'0');
                    else if (digit >= L'a' && digit <= L'f') value = static_cast<unsigned int>(digit - L'a' + 10);
                    else if (digit >= L'A' && digit <= L'F') value = static_cast<unsigned int>(digit - L'A' + 10);
                    else return false;
                    code = (code << 4) | value;
                }
                if (decoded) local.push_back(static_cast<wchar_t>(code));
                i += 4;
                break;
            }
            default: return false;
        }
    }
    return false;
}

inline size_t SkipJsonValue(std::wstring_view json, size_t position, unsigned depth = 0) {
    position = SkipWhitespace(json, position);
    if (position >= json.size() || depth > 32) return std::wstring_view::npos;

    if (json[position] == L'"') {
        size_t end = 0;
        return ParseStringToken(json, position, end) ? end : std::wstring_view::npos;
    }

    if (json[position] == L'{' || json[position] == L'[') {
        const bool object = json[position] == L'{';
        const wchar_t closing = object ? L'}' : L']';
        size_t i = SkipWhitespace(json, position + 1);
        if (i < json.size() && json[i] == closing) return i + 1;
        while (i < json.size()) {
            if (object) {
                size_t key_end = 0;
                if (!ParseStringToken(json, i, key_end)) return std::wstring_view::npos;
                i = SkipWhitespace(json, key_end);
                if (i >= json.size() || json[i] != L':') return std::wstring_view::npos;
                i = SkipWhitespace(json, i + 1);
            }
            const size_t value_end = SkipJsonValue(json, i, depth + 1);
            if (value_end == std::wstring_view::npos) return value_end;
            i = SkipWhitespace(json, value_end);
            if (i >= json.size()) return std::wstring_view::npos;
            if (json[i] == closing) return i + 1;
            if (json[i] != L',') return std::wstring_view::npos;
            i = SkipWhitespace(json, i + 1);
        }
        return std::wstring_view::npos;
    }

    size_t i = position;
    while (i < json.size() && json[i] != L',' && json[i] != L']' &&
        json[i] != L'}' && json[i] != L' ' && json[i] != L'\t' &&
        json[i] != L'\r' && json[i] != L'\n') {
        ++i;
    }
    if (i == position) return std::wstring_view::npos;
    const auto token = json.substr(position, i - position);
    if (token == L"true" || token == L"false" || token == L"null") return i;

    // Validate a JSON number rather than accepting arbitrary unquoted text.
    size_t n = 0;
    if (token[n] == L'-') ++n;
    if (n >= token.size()) return std::wstring_view::npos;
    if (token[n] == L'0') {
        ++n;
    } else {
        if (token[n] < L'1' || token[n] > L'9') return std::wstring_view::npos;
        while (n < token.size() && token[n] >= L'0' && token[n] <= L'9') ++n;
    }
    if (n < token.size() && token[n] == L'.') {
        ++n;
        const size_t first = n;
        while (n < token.size() && token[n] >= L'0' && token[n] <= L'9') ++n;
        if (n == first) return std::wstring_view::npos;
    }
    if (n < token.size() && (token[n] == L'e' || token[n] == L'E')) {
        ++n;
        if (n < token.size() && (token[n] == L'+' || token[n] == L'-')) ++n;
        const size_t first = n;
        while (n < token.size() && token[n] >= L'0' && token[n] <= L'9') ++n;
        if (n == first) return std::wstring_view::npos;
    }
    return n == token.size() ? i : std::wstring_view::npos;
}

inline bool ExtractJsonProperty(std::wstring_view object, std::wstring_view wanted,
    std::wstring& raw_value) {
    raw_value.clear();
    size_t i = SkipWhitespace(object, 0);
    if (i >= object.size() || object[i] != L'{') return false;
    i = SkipWhitespace(object, i + 1);
    if (i < object.size() && object[i] == L'}') return false;
    while (i < object.size()) {
        std::wstring key;
        size_t key_end = 0;
        if (!ParseStringToken(object, i, key_end, &key)) return false;
        i = SkipWhitespace(object, key_end);
        if (i >= object.size() || object[i] != L':') return false;
        const size_t value_start = SkipWhitespace(object, i + 1);
        const size_t value_end = SkipJsonValue(object, value_start);
        if (value_end == std::wstring_view::npos) return false;
        if (key == wanted) {
            raw_value.assign(object.substr(value_start, value_end - value_start));
            return true;
        }
        i = SkipWhitespace(object, value_end);
        if (i >= object.size()) return false;
        if (object[i] == L'}') return false;
        if (object[i] != L',') return false;
        i = SkipWhitespace(object, i + 1);
    }
    return false;
}

inline std::wstring_view TrimJson(std::wstring_view value) noexcept {
    const size_t start = SkipWhitespace(value, 0);
    size_t end = value.size();
    while (end > start && (value[end - 1] == L' ' || value[end - 1] == L'\t' ||
        value[end - 1] == L'\r' || value[end - 1] == L'\n')) --end;
    return value.substr(start, end - start);
}

inline bool DefaultFromManifest(std::wstring_view manifest, std::wstring_view setting_id,
    std::wstring& raw_value) {
    raw_value.clear();
    std::wstring settings_raw;
    if (!ExtractJsonProperty(manifest, L"settings", settings_raw)) return false;
    const auto settings = TrimJson(settings_raw);
    if (settings.empty() || settings.front() != L'[') return false;

    size_t i = SkipWhitespace(settings, 1);
    if (i < settings.size() && settings[i] == L']') return false;
    while (i < settings.size()) {
        const size_t end = SkipJsonValue(settings, i);
        if (end == std::wstring_view::npos) return false;
        const auto spec = settings.substr(i, end - i);
        std::wstring raw_id;
        std::wstring id;
        if (ExtractJsonProperty(spec, L"id", raw_id) &&
            ParseStringToken(TrimJson(raw_id), 0, end /* overwritten below */, &id) &&
            id == setting_id) {
            return ExtractJsonProperty(spec, L"default", raw_value);
        }
        i = SkipWhitespace(settings, end);
        if (i >= settings.size() || settings[i] == L']') return false;
        if (settings[i] != L',') return false;
        i = SkipWhitespace(settings, i + 1);
    }
    return false;
}

inline bool ReadRawSetting(const wchar_t* key, std::wstring& raw_value) noexcept {
    raw_value.clear();
    if (!key || !*key) return false;
    try {
        const auto folder = wmod::PackageDirectory();
        if (folder.empty()) return false;
        std::wstring contents;
        if (ReadUtf8File(folder / L"settings.json", contents) &&
            ExtractJsonProperty(contents, key, raw_value)) {
            return true;
        }
        if (!ReadUtf8File(folder / L"manifest.json", contents)) return false;
        return DefaultFromManifest(contents, key, raw_value);
    } catch (...) {
        raw_value.clear();
        return false;
    }
}

inline bool DecodeJsonString(std::wstring_view raw, std::wstring& decoded) {
    decoded.clear();
    raw = TrimJson(raw);
    size_t end = 0;
    return ParseStringToken(raw, 0, end, &decoded) && end == raw.size();
}

inline bool ValidStorageKey(const wchar_t* key) noexcept {
    if (!key || !*key) return false;
    size_t length = 0;
    for (const wchar_t* p = key; *p; ++p) {
        if (++length > kMaxSettingNameChars || *p < 0x20 ||
            *p == L'[' || *p == L']' || *p == L'=' || *p == L';') return false;
    }
    return true;
}

inline std::filesystem::path ValuesPath() {
    const auto folder = wmod::PackageDirectory();
    return folder.empty() ? std::filesystem::path{} : folder / L"wh_values.ini";
}

inline bool FormatKey(PCWSTR format, va_list args, std::wstring& output) noexcept {
    output.clear();
    if (!format) return false;
    try {
        std::vector<wchar_t> buffer(kMaxSettingNameChars + 1, L'\0');
        va_list copy;
        va_copy(copy, args);
        const int count = _vsnwprintf_s(buffer.data(), buffer.size(), _TRUNCATE, format, copy);
        va_end(copy);
        if (count < 0) {
            // A non-format literal can still be a valid key. The bounded formatter
            // reports truncation with -1, which is rejected rather than truncated.
            return false;
        }
        output.assign(buffer.data(), static_cast<size_t>(count));
        return ValidStorageKey(output.c_str());
    } catch (...) {
        output.clear();
        return false;
    }
}

inline int SettingInt(PCWSTR key) noexcept {
    std::wstring raw;
    if (!ReadRawSetting(key, raw)) return 0;
    const auto value = TrimJson(raw);
    if (value.empty() || value.front() == L'"') return 0;
    if (value == L"true") return 1;
    if (value == L"false" || value == L"null") return 0;
    wchar_t* end = nullptr;
    errno = 0;
    const long long parsed = _wcstoi64(value.data(), &end, 10);
    if (errno != 0 || end == value.data() ||
        static_cast<size_t>(end - value.data()) != value.size() ||
        parsed < std::numeric_limits<int>::min() || parsed > std::numeric_limits<int>::max()) return 0;
    return static_cast<int>(parsed);
}

inline PCWSTR SettingString(PCWSTR key) noexcept {
    std::wstring raw, decoded;
    if (!ReadRawSetting(key, raw) || !DecodeJsonString(raw, decoded) ||
        decoded.size() > kMaxStorageStringChars) {
        return nullptr;
    }
    auto* result = new (std::nothrow) wchar_t[decoded.size() + 1];
    if (!result) return nullptr;
    std::copy(decoded.begin(), decoded.end(), result);
    result[decoded.size()] = L'\0';
    return result;
}

inline void Logf(PCWSTR format, ...) noexcept {
    if (!format) return;
    wchar_t buffer[kMaxLogChars + 1]{};
    va_list args;
    va_start(args, format);
    _vsnwprintf_s(buffer, _countof(buffer), _TRUNCATE, format, args);
    va_end(args);
    (void)wmod::Log(L"info", buffer);
}

} // namespace wmod::windhawk_compat

// Windhawk-style logging syntax is source-compatible for ordinary wide-string
// printf-style messages. It writes to 00047's bounded native-mod log file.
#ifndef Wh_Log
#define Wh_Log(...) ::wmod::windhawk_compat::Logf(__VA_ARGS__)
#endif

// Supported settings APIs: values come from settings.json, with defaults read
// from manifest.json when settings have not yet been saved by 00047's editor.
// These are inline helpers, not imports from or a complete implementation of
// the Windhawk engine.
inline int Wh_GetIntSetting(PCWSTR valueName, ...) {
    va_list args;
    va_start(args, valueName);
    std::wstring key;
    const bool ok = ::wmod::windhawk_compat::FormatKey(valueName, args, key);
    va_end(args);
    return ok ? ::wmod::windhawk_compat::SettingInt(key.c_str()) : 0;
}

inline PCWSTR Wh_GetStringSetting(PCWSTR valueName, ...) {
    va_list args;
    va_start(args, valueName);
    std::wstring key;
    const bool ok = ::wmod::windhawk_compat::FormatKey(valueName, args, key);
    va_end(args);
    return ok ? ::wmod::windhawk_compat::SettingString(key.c_str()) : nullptr;
}

inline void Wh_FreeStringSetting(PCWSTR value) {
    delete[] const_cast<PWSTR>(value);
}

inline int Wh_GetIntValue(PCWSTR valueName, int defaultValue) {
    if (!::wmod::windhawk_compat::ValidStorageKey(valueName)) return defaultValue;
    const auto path = ::wmod::windhawk_compat::ValuesPath();
    if (path.empty()) return defaultValue;
    return static_cast<int>(GetPrivateProfileIntW(L"values", valueName, defaultValue, path.c_str()));
}

inline BOOL Wh_SetIntValue(PCWSTR valueName, int value) {
    if (!::wmod::windhawk_compat::ValidStorageKey(valueName)) return FALSE;
    const auto path = ::wmod::windhawk_compat::ValuesPath();
    if (path.empty()) return FALSE;
    wchar_t text[32]{};
    if (swprintf_s(text, _countof(text), L"%d", value) < 0) return FALSE;
    return WritePrivateProfileStringW(L"values", valueName, text, path.c_str());
}

inline size_t Wh_GetStringValue(PCWSTR valueName, PWSTR stringBuffer, size_t bufferChars) {
    if (!stringBuffer || bufferChars == 0) return 0;
    stringBuffer[0] = L'\0';
    if (bufferChars > std::numeric_limits<DWORD>::max() ||
        !::wmod::windhawk_compat::ValidStorageKey(valueName)) return 0;
    const auto path = ::wmod::windhawk_compat::ValuesPath();
    if (path.empty()) return 0;
    const DWORD copied = GetPrivateProfileStringW(
        L"values", valueName, L"", stringBuffer, static_cast<DWORD>(bufferChars), path.c_str());
    if (copied >= bufferChars - 1) {
        stringBuffer[0] = L'\0';
        return 0;
    }
    return static_cast<size_t>(copied);
}

inline BOOL Wh_SetStringValue(PCWSTR valueName, PCWSTR value) {
    if (!::wmod::windhawk_compat::ValidStorageKey(valueName) || !value) return FALSE;
    size_t length = 0;
    while (value[length]) {
        if (++length > ::wmod::windhawk_compat::kMaxStorageStringChars) return FALSE;
    }
    const auto path = ::wmod::windhawk_compat::ValuesPath();
    if (path.empty()) return FALSE;
    return WritePrivateProfileStringW(L"values", valueName, value, path.c_str());
}

#endif // WMOD_00047_WINDHAWK_NONHOOK_COMPAT_H
