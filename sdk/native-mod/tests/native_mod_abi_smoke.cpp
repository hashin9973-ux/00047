#include <Windows.h>

#include <cstdint>
#include <cstdio>

namespace {
using GetApiVersionFn = std::uint32_t (WINAPI*)();
using BoolExportFn = BOOL (WINAPI*)();
using VoidExportFn = void (WINAPI*)();
}

#define CHECK(expression) do { \
    if (!(expression)) { \
        std::fprintf(stderr, "CHECK failed at %s:%d: %s\n", __FILE__, __LINE__, #expression); \
        if (module) FreeLibrary(module); \
        return 1; \
    } \
} while (0)

int wmain(int argc, wchar_t** argv) {
    HMODULE module = nullptr;
    if (argc != 2) {
        std::fprintf(stderr, "Usage: 00047_native_mod_abi_smoke <module.dll>\n");
        return 2;
    }

    module = LoadLibraryW(argv[1]);
    CHECK(module != nullptr);

    const auto getApiVersion = reinterpret_cast<GetApiVersionFn>(
        GetProcAddress(module, "WmodGetApiVersion"));
    const auto initialize = reinterpret_cast<BoolExportFn>(
        GetProcAddress(module, "WmodInitializeV1"));
    const auto shutdown = reinterpret_cast<VoidExportFn>(
        GetProcAddress(module, "WmodShutdownV1"));
    const auto isInitialized = reinterpret_cast<BoolExportFn>(
        GetProcAddress(module, "WmodIsInitializedV1"));
    const auto canUnload = reinterpret_cast<BoolExportFn>(
        GetProcAddress(module, "WmodCanUnloadV1"));

    CHECK(getApiVersion != nullptr);
    CHECK(initialize != nullptr);
    CHECK(shutdown != nullptr);
    CHECK(isInitialized != nullptr);
    CHECK(canUnload != nullptr);
    CHECK(getApiVersion() == 2);
    CHECK(isInitialized() == FALSE);
    CHECK(canUnload() == TRUE);

    CHECK(initialize() != FALSE);
    CHECK(isInitialized() != FALSE);
    CHECK(canUnload() == FALSE);

    shutdown();
    CHECK(isInitialized() == FALSE);
    CHECK(canUnload() == TRUE);
    CHECK(FreeLibrary(module) != FALSE);
    std::puts("Native-mod lifecycle ABI smoke test passed.");
    return 0;
}
