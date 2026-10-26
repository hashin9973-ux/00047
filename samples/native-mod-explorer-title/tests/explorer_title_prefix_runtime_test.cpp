#include "00047_mod_api.h"

#include <Windows.h>

#include <cstdio>
#include <cstdint>
#include <filesystem>
#include <string>
#include <vector>

#define CHECK(expression) do { \
    if (!(expression)) { \
        std::fprintf(stderr, "CHECK failed at %s:%d: %s\n", __FILE__, __LINE__, #expression); \
        return __LINE__; \
    } \
} while (0)

using GetApiVersionFn = std::uint32_t (WINAPI*)();
using BoolFn = BOOL (WINAPI*)();
using ShutdownFn = void (WINAPI*)();

namespace {
constexpr wchar_t kTargetClass[] = L"CabinetWClass";
constexpr wchar_t kOtherClass[] = L"00047_ExplorerTitlePrefixUnrelated";
constexpr wchar_t kExpectedPrefix[] = L"[00047] ";

bool RegisterTestClass(const wchar_t* name, HINSTANCE instance) {
    WNDCLASSW windowClass{};
    windowClass.lpfnWndProc = DefWindowProcW;
    windowClass.hInstance = instance;
    windowClass.lpszClassName = name;
    if (RegisterClassW(&windowClass)) return true;
    // Never silently reuse a pre-registered class with an unknown window proc.
    return false;
}

std::wstring ReadCaption(HWND hwnd) {
    const int length = GetWindowTextLengthW(hwnd);
    if (length < 0 || length > 32768) return {};
    std::vector<wchar_t> buffer(static_cast<size_t>(length) + 1, L'\0');
    const int copied = GetWindowTextW(hwnd, buffer.data(), static_cast<int>(buffer.size()));
    if (copied < 0 || copied >= static_cast<int>(buffer.size())) return {};
    return std::wstring(buffer.data(), static_cast<size_t>(copied));
}
}

int wmain(int argc, wchar_t** argv) {
    if (argc != 3) {
        std::fwprintf(stderr, L"Usage: explorer_title_prefix_runtime_test.exe <mod-dll> <manifest-json>\n");
        return 2;
    }

    const auto root = std::filesystem::temp_directory_path() /
        (L"00047-explorer-title-prefix-test-" + std::to_wstring(GetCurrentProcessId()) +
            L"-" + std::to_wstring(GetTickCount64()));
    const auto bin = root / L"bin";
    std::error_code filesystemError;
    std::filesystem::create_directories(bin, filesystemError);
    CHECK(!filesystemError);

    const auto testDll = bin / L"00047_explorer_title_prefix_mod.dll";
    const auto testManifest = root / L"manifest.json";
    CHECK(CopyFileW(argv[1], testDll.c_str(), FALSE) != FALSE);
    CHECK(CopyFileW(argv[2], testManifest.c_str(), FALSE) != FALSE);

    const HINSTANCE instance = GetModuleHandleW(nullptr);
    CHECK(RegisterTestClass(kTargetClass, instance));
    CHECK(RegisterTestClass(kOtherClass, instance));

    HWND targetWindow = CreateWindowExW(0, kTargetClass, L"File Explorer",
        WS_OVERLAPPED, 0, 0, 200, 100, nullptr, nullptr, instance, nullptr);
    CHECK(targetWindow != nullptr);
    HWND otherWindow = CreateWindowExW(0, kOtherClass, L"Unrelated window",
        WS_OVERLAPPED, 0, 0, 200, 100, nullptr, nullptr, instance, nullptr);
    CHECK(otherWindow != nullptr);
    CHECK(ReadCaption(targetWindow) == L"File Explorer");

    HMODULE module = LoadLibraryW(testDll.c_str());
    CHECK(module != nullptr);
    const auto getApiVersion = reinterpret_cast<GetApiVersionFn>(
        GetProcAddress(module, "WmodGetApiVersion"));
    const auto initialize = reinterpret_cast<BoolFn>(GetProcAddress(module, "WmodInitializeV1"));
    const auto shutdown = reinterpret_cast<ShutdownFn>(GetProcAddress(module, "WmodShutdownV1"));
    const auto isInitialized = reinterpret_cast<BoolFn>(GetProcAddress(module, "WmodIsInitializedV1"));
    const auto canUnload = reinterpret_cast<BoolFn>(GetProcAddress(module, "WmodCanUnloadV1"));
    CHECK(getApiVersion && initialize && shutdown && isInitialized && canUnload);
    CHECK(getApiVersion() == WMOD_00047_ABI_VERSION);
    CHECK(initialize() == TRUE);
    CHECK(isInitialized() == TRUE);
    CHECK(ReadCaption(targetWindow) == std::wstring(kExpectedPrefix) + L"File Explorer");
    CHECK(ReadCaption(otherWindow) == L"Unrelated window");

    CHECK(SetWindowTextW(targetWindow, L"Documents") == TRUE);
    CHECK(ReadCaption(targetWindow) == std::wstring(kExpectedPrefix) + L"Documents");
    // Repeated requests for the same unprefixed title must not duplicate the prefix.
    CHECK(SetWindowTextW(targetWindow, L"Documents") == TRUE);
    CHECK(ReadCaption(targetWindow) == std::wstring(kExpectedPrefix) + L"Documents");

    CHECK(SetWindowTextW(otherWindow, L"Settings") == TRUE);
    CHECK(ReadCaption(otherWindow) == L"Settings");

    shutdown();
    CHECK(isInitialized() == FALSE);
    CHECK(ReadCaption(targetWindow) == L"Documents");
    CHECK(ReadCaption(otherWindow) == L"Settings");
    CHECK(canUnload() == TRUE);

    CHECK(FreeLibrary(module) != FALSE);
    CHECK(DestroyWindow(targetWindow) != FALSE);
    CHECK(DestroyWindow(otherWindow) != FALSE);
    CHECK(UnregisterClassW(kTargetClass, instance) != FALSE);
    CHECK(UnregisterClassW(kOtherClass, instance) != FALSE);
    std::filesystem::remove_all(root, filesystemError);
    return 0;
}
