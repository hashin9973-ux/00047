#pragma once
#ifndef WMOD_00047_MOD_SERVICES_H
#define WMOD_00047_MOD_SERVICES_H
// Optional settings and structured-log helpers for the 00047 native-mod SDK.
// No function hooking, sandboxing, or Windhawk API compatibility is provided here.
#include <Windows.h>
#include <cstdio>
#include <cwchar>
#include <filesystem>
#include <string>
#include <system_error>
#include <vector>

namespace wmod {
inline std::filesystem::path PackageDirectory() noexcept {
    try {
        static const int module_marker = 0;
        HMODULE module = nullptr;
        const DWORD flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
        if (!GetModuleHandleExW(flags, reinterpret_cast<LPCWSTR>(&module_marker), &module)) return {};
        std::vector<wchar_t> buffer(32768);
        const DWORD length = GetModuleFileNameW(module, buffer.data(), static_cast<DWORD>(buffer.size()));
        if (length == 0 || length >= buffer.size()) return {};
        std::filesystem::path current = std::filesystem::path(std::wstring(buffer.data(), length)).parent_path();
        for (int i = 0; i < 8 && !current.empty(); ++i) {
            std::error_code ec;
            if (std::filesystem::is_regular_file(current / L"manifest.json", ec) && !ec) return current;
            const auto parent = current.parent_path();
            if (parent.empty() || parent == current) break;
            current = parent;
        }
    } catch (...) { return {}; }
    return {};
}

inline std::filesystem::path SettingsPath() noexcept {
    const auto folder = PackageDirectory();
    return folder.empty() ? std::filesystem::path{} : folder / L"settings.json";
}

inline bool ReadSettingsJson(std::string& json_utf8) noexcept {
    try {
        const auto path = SettingsPath();
        if (path.empty()) return false;
        FILE* file = nullptr;
        if (_wfopen_s(&file, path.c_str(), L"rb") != 0 || !file) return false;
        if (_fseeki64(file, 0, SEEK_END) != 0) { std::fclose(file); return false; }
        const auto size = _ftelli64(file);
        if (size < 0 || size > 256 * 1024 || _fseeki64(file, 0, SEEK_SET) != 0) { std::fclose(file); return false; }
        json_utf8.resize(static_cast<size_t>(size));
        const size_t count = std::fread(json_utf8.data(), 1, json_utf8.size(), file);
        std::fclose(file);
        if (count != json_utf8.size()) { json_utf8.clear(); return false; }
        return true;
    } catch (...) { json_utf8.clear(); return false; }
}

// Resolve an export from a module that is already loaded in this process.
// These helpers never load arbitrary DLLs or download symbols; the caller owns any
// returned pointer's lifetime and must ensure the target module remains loaded.
inline HMODULE ModuleHandle(const wchar_t* module_name) noexcept {
    if (!module_name || !*module_name) return nullptr;
    return GetModuleHandleW(module_name);
}

inline FARPROC ResolveSymbol(HMODULE module, const char* symbol_name) noexcept {
    if (!module || !symbol_name || !*symbol_name) return nullptr;
    return GetProcAddress(module, symbol_name);
}

inline FARPROC ResolveSymbol(const wchar_t* module_name, const char* symbol_name) noexcept {
    return ResolveSymbol(ModuleHandle(module_name), symbol_name);
}

inline std::string Utf8(const std::wstring& text) {
    if (text.empty()) return {};
    const int length = WideCharToMultiByte(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), nullptr, 0, nullptr, nullptr);
    if (length <= 0) return {};
    std::string output(static_cast<size_t>(length), '\0');
    if (WideCharToMultiByte(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), output.data(), length, nullptr, nullptr) != length) return {};
    return output;
}

inline std::wstring JsonString(const wchar_t* text) {
    static constexpr wchar_t hex[] = L"0123456789abcdef";
    std::wstring out = L"\"";
    if (text) for (const wchar_t* p = text; *p; ++p) {
        const wchar_t ch = *p;
        switch (ch) {
            case L'"': out += L"\\\""; break;
            case L'\\': out += L"\\\\"; break;
            case L'\b': out += L"\\b"; break;
            case L'\f': out += L"\\f"; break;
            case L'\n': out += L"\\n"; break;
            case L'\r': out += L"\\r"; break;
            case L'\t': out += L"\\t"; break;
            default:
                if (ch < 0x20) { out += L"\\u00"; out += hex[(ch >> 4) & 0xF]; out += hex[ch & 0xF]; }
                else out += ch;
        }
    }
    out += L'"';
    return out;
}

inline bool Log(const wchar_t* level, const wchar_t* message) noexcept {
    try {
        const auto folder = PackageDirectory();
        if (folder.empty()) return false;
        SYSTEMTIME now{};
        GetSystemTime(&now);
        wchar_t timestamp[40]{};
        std::swprintf(timestamp, _countof(timestamp), L"%04u-%02u-%02uT%02u:%02u:%02u.%03uZ",
            now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond, now.wMilliseconds);
        std::wstring safe_message = message ? message : L"";
        if (safe_message.size() > 8192) safe_message.resize(8192);
        std::wstring safe_level = level ? level : L"info";
        if (safe_level.size() > 32) safe_level.resize(32);
        const std::wstring line = L"{\"timestamp\":" + JsonString(timestamp) +
            L",\"level\":" + JsonString(safe_level.c_str()) +
            L",\"message\":" + JsonString(safe_message.c_str()) + L"}\r\n";
        const std::string bytes = Utf8(line);
        if (bytes.empty()) return false;
        const auto path = folder / L"mod.log";
        HANDLE file = CreateFileW(path.c_str(), FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            nullptr, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
        if (file == INVALID_HANDLE_VALUE) return false;
        DWORD written = 0;
        const BOOL ok = WriteFile(file, bytes.data(), static_cast<DWORD>(bytes.size()), &written, nullptr);
        CloseHandle(file);
        return ok != FALSE && written == bytes.size();
    } catch (...) { return false; }
}
} // namespace wmod
#endif
