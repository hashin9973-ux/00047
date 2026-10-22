#include "windhawk_hook_compat.h"

#include <Windows.h>
#include <atomic>
#include <cassert>
#include <thread>

using TestFunction = int(__cdecl*)(int);
static void* g_originalAddress = nullptr;
static std::atomic<bool> g_holdDetour{false};
static std::atomic<bool> g_insideDetour{false};

extern "C" __declspec(noinline) int __cdecl HookTestTarget(int value) {
    volatile int result = value + 2;
    return result;
}

extern "C" __declspec(noinline) int __cdecl HookTestDetour(int value) {
    if (g_holdDetour.load(std::memory_order_acquire)) {
        g_insideDetour.store(true, std::memory_order_release);
        while (g_holdDetour.load(std::memory_order_acquire)) {
            Sleep(1);
        }
        // Do not invoke the trampoline here: shutdown removes it while this
        // test call remains in flight, and the module must be kept mapped.
        return 123;
    }
    auto original = reinterpret_cast<TestFunction>(g_originalAddress);
    return original(value) + 10;
}

int main() {
    assert(HookTestTarget(5) == 7);

    WH_DISASM_RESULT instruction{};
    assert(Wh_Disasm(reinterpret_cast<void*>(&HookTestTarget), &instruction) == TRUE);
    assert(instruction.length > 0 && instruction.length <= 15);
    assert(instruction.text[0] != '\0');

    assert(Wh_SetFunctionHook(
        reinterpret_cast<void*>(&HookTestTarget),
        reinterpret_cast<void*>(&HookTestDetour),
        &g_originalAddress) == TRUE);
    assert(g_originalAddress != nullptr);
    assert(HookTestTarget(5) == 7); // registration is deferred until apply

    assert(Wh_ApplyHookOperations() == TRUE);
    assert(HookTestTarget(5) == 17);

    assert(Wh_RemoveFunctionHook(reinterpret_cast<void*>(&HookTestTarget)) == TRUE);
    assert(HookTestTarget(5) == 17); // removal is also queued
    assert(Wh_ApplyHookOperations() == TRUE);
    assert(HookTestTarget(5) == 7);
    assert(wmod::windhawk_hook_compat::CanUnload() == FALSE); // lifecycle is still accepting hooks

    // Exercise the critical unload race: a detour may still be executing even
    // after its entry point is restored. The quiescence query must fail closed
    // until that callback and its return path have left the module image.
    assert(Wh_SetFunctionHook(
        reinterpret_cast<void*>(&HookTestTarget),
        reinterpret_cast<void*>(&HookTestDetour),
        &g_originalAddress) == TRUE);
    assert(Wh_ApplyHookOperations() == TRUE);
    g_insideDetour.store(false, std::memory_order_release);
    g_holdDetour.store(true, std::memory_order_release);
    int result = 0;
    std::thread caller([&] { result = HookTestTarget(9); });

    for (int i = 0; i < 2000 && !g_insideDetour.load(std::memory_order_acquire); ++i) {
        Sleep(1);
    }
    assert(g_insideDetour.load(std::memory_order_acquire));

    wmod::windhawk_hook_compat::ShutdownHookOperations();
    assert(wmod::windhawk_hook_compat::CanUnload() == FALSE);

    g_holdDetour.store(false, std::memory_order_release);
    caller.join();
    assert(result == 123);
    assert(wmod::windhawk_hook_compat::CanUnload() == TRUE);
    return 0;
}
