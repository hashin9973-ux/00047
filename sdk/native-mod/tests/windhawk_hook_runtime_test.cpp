#include <Windows.h>

#include <atomic>
#include <cstdio>
#include <thread>

struct TestDisasmResult { size_t length; char text[96]; };
using TargetFn = int (WINAPI*)(int);
using DisasmFn = BOOL (WINAPI*)(TestDisasmResult*);
using SimpleBoolFn = BOOL (WINAPI*)();
using VoidFn = void (WINAPI*)();
using HoldFn = void (WINAPI*)(BOOL);
using InsideFn = BOOL (WINAPI*)();

#define CHECK(expression) do { \
    if (!(expression)) { \
        std::fprintf(stderr, "CHECK failed at %s:%d: %s\n", __FILE__, __LINE__, #expression); \
        return __LINE__; \
    } \
} while (0)

int wmain(int argc, wchar_t** argv) {
    if (argc != 2) {
        std::fwprintf(stderr, L"Usage: 00047_windhawk_hook_runtime_test.exe <hook-smoke-dll>\n");
        return 2;
    }

    HMODULE module = LoadLibraryW(argv[1]);
    CHECK(module != nullptr);
    auto target = reinterpret_cast<TargetFn>(GetProcAddress(module, "HookRuntimeTarget"));
    auto disasm = reinterpret_cast<DisasmFn>(GetProcAddress(module, "HookRuntimeDisasm"));
    auto begin = reinterpret_cast<SimpleBoolFn>(GetProcAddress(module, "HookRuntimeBegin"));
    auto install = reinterpret_cast<SimpleBoolFn>(GetProcAddress(module, "HookRuntimeInstall"));
    auto apply = reinterpret_cast<SimpleBoolFn>(GetProcAddress(module, "HookRuntimeApply"));
    auto remove = reinterpret_cast<SimpleBoolFn>(GetProcAddress(module, "HookRuntimeRemove"));
    auto shutdown = reinterpret_cast<VoidFn>(GetProcAddress(module, "HookRuntimeShutdown"));
    auto canUnload = reinterpret_cast<SimpleBoolFn>(GetProcAddress(module, "HookRuntimeCanUnload"));
    auto holdDetour = reinterpret_cast<HoldFn>(GetProcAddress(module, "HookRuntimeHoldDetour"));
    auto insideDetour = reinterpret_cast<InsideFn>(GetProcAddress(module, "HookRuntimeInsideDetour"));
    CHECK(target && disasm && begin && install && apply && remove &&
          shutdown && canUnload && holdDetour && insideDetour);

    CHECK(target(5) == 7);
    TestDisasmResult disasmResult{};
    CHECK(disasm(&disasmResult) == TRUE);
    CHECK(disasmResult.length > 0 && disasmResult.length <= 15);
    CHECK(disasmResult.text[0] != '\0');
    CHECK(begin() == TRUE);
    CHECK(install() == TRUE);
    CHECK(target(5) == 7); // registering a hook is deferred until apply
    CHECK(apply() == TRUE);
    CHECK(target(5) == 17);

    CHECK(remove() == TRUE);
    CHECK(target(5) == 17); // removals are queued too
    CHECK(apply() == TRUE);
    CHECK(target(5) == 7);
    CHECK(canUnload() == FALSE); // lifecycle still accepts new hooks
    shutdown();
    CHECK(canUnload() == TRUE);

    // Re-enter an initialized hook lifecycle and hold a detour on a second
    // thread while shutdown runs. The module must not become unloadable until
    // that thread returns out of its detour and the DLL image.
    CHECK(begin() == TRUE);
    CHECK(install() == TRUE);
    CHECK(apply() == TRUE);
    holdDetour(TRUE);
    std::atomic<int> callResult{0};
    std::thread caller([&] {
        callResult.store(target(9), std::memory_order_release);
    });

    bool entered = false;
    for (int i = 0; i < 2000; ++i) {
        if (insideDetour() == TRUE) {
            entered = true;
            break;
        }
        Sleep(1);
    }
    if (!entered) {
        holdDetour(FALSE);
        caller.join();
        shutdown();
        FreeLibrary(module);
        std::fprintf(stderr, "timed out waiting for the detour thread to enter\n");
        return 30;
    }

    shutdown();
    const BOOL unsafeWhileActive = canUnload();
    holdDetour(FALSE);
    caller.join();

    if (unsafeWhileActive != FALSE) {
        FreeLibrary(module);
        std::fprintf(stderr, "quiescence check incorrectly approved unload while detour was active\n");
        return 31;
    }
    if (callResult.load(std::memory_order_acquire) != 123) {
        FreeLibrary(module);
        std::fprintf(stderr, "blocked detour did not return the expected sentinel value\n");
        return 32;
    }
    if (canUnload() != TRUE) {
        FreeLibrary(module);
        std::fprintf(stderr, "quiescence check did not recover after detour thread exited\n");
        return 33;
    }

    CHECK(FreeLibrary(module) != FALSE);
    return 0;
}
