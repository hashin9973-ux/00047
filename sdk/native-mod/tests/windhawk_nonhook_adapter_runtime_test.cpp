#include <Windows.h>

#include <cstdio>

using SimpleBoolFn = BOOL (WINAPI*)();
using VoidFn = void (WINAPI*)();
using SetAllowedFn = void (WINAPI*)(BOOL);
using StepFn = int (WINAPI*)();

#define CHECK(expression) do { \
    if (!(expression)) { \
        std::fprintf(stderr, "CHECK failed at %s:%d: %s\n", __FILE__, __LINE__, #expression); \
        return __LINE__; \
    } \
} while (0)

int wmain(int argc, wchar_t** argv) {
    if (argc != 2) {
        std::fwprintf(stderr, L"Usage: 00047_windhawk_nonhook_adapter_runtime_test.exe <adapter-smoke-dll>\n");
        return 2;
    }

    HMODULE module = LoadLibraryW(argv[1]);
    CHECK(module != nullptr);

    auto initialize = reinterpret_cast<SimpleBoolFn>(GetProcAddress(module, "WmodInitializeV1"));
    auto shutdown = reinterpret_cast<VoidFn>(GetProcAddress(module, "WmodShutdownV1"));
    auto isInitialized = reinterpret_cast<SimpleBoolFn>(GetProcAddress(module, "WmodIsInitializedV1"));
    auto canUnload = reinterpret_cast<SimpleBoolFn>(GetProcAddress(module, "WmodCanUnloadV1"));
    auto setInitAllowed = reinterpret_cast<SetAllowedFn>(GetProcAddress(module, "AdapterSmokeSetInitAllowed"));
    auto getLifecycleStep = reinterpret_cast<StepFn>(GetProcAddress(module, "AdapterSmokeLifecycleStep"));
    CHECK(initialize && shutdown && isInitialized && canUnload && setInitAllowed && getLifecycleStep);

    // Initialization rejection must not invoke after-init, before-uninit, or
    // uninit callbacks. It must also leave the hook lifecycle safely retryable.
    setInitAllowed(FALSE);
    CHECK(initialize() == FALSE);
    CHECK(isInitialized() == FALSE);
    CHECK(getLifecycleStep() == 0);
    CHECK(canUnload() == TRUE);

    setInitAllowed(TRUE);
    CHECK(initialize() == TRUE);
    CHECK(isInitialized() == TRUE);
    CHECK(getLifecycleStep() == 2);

    // Repeated initialization while active is idempotent and must not duplicate
    // lifecycle callbacks or hook-engine state.
    CHECK(initialize() == TRUE);
    CHECK(getLifecycleStep() == 2);

    shutdown();
    CHECK(isInitialized() == FALSE);
    CHECK(getLifecycleStep() == 4);
    CHECK(canUnload() == TRUE);

    // Shutdown from an already stopped state is idempotent.
    shutdown();
    CHECK(isInitialized() == FALSE);
    CHECK(canUnload() == TRUE);

    // Exercise repeated start/stop cycles and then another failed init, catching
    // stale lifecycle state that can otherwise prevent later enable operations.
    for (int iteration = 0; iteration < 3; ++iteration) {
        CHECK(initialize() == TRUE);
        CHECK(isInitialized() == TRUE);
        CHECK(getLifecycleStep() == 2);
        shutdown();
        CHECK(isInitialized() == FALSE);
        CHECK(getLifecycleStep() == 4);
        CHECK(canUnload() == TRUE);
    }

    setInitAllowed(FALSE);
    CHECK(initialize() == FALSE);
    CHECK(isInitialized() == FALSE);
    CHECK(getLifecycleStep() == 4);
    CHECK(canUnload() == TRUE);

    CHECK(FreeLibrary(module) != FALSE);
    std::puts("Non-hook lifecycle adapter runtime test passed.");
    return 0;
}
