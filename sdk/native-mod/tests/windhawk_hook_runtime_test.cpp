#include "windhawk_hook_compat.h"

#include <cassert>

using TestFunction = int(__cdecl*)(int);
static void* g_originalAddress = nullptr;

extern "C" __declspec(noinline) int __cdecl HookTestTarget(int value) {
    volatile int result = value + 2;
    return result;
}

extern "C" __declspec(noinline) int __cdecl HookTestDetour(int value) {
    auto original = reinterpret_cast<TestFunction>(g_originalAddress);
    return original(value) + 10;
}

int main() {
    assert(HookTestTarget(5) == 7);
    WH_DISASM_RESULT instruction{};
    assert(Wh_Disasm(reinterpret_cast<void*>(&HookTestTarget), &instruction) == TRUE);
    assert(instruction.length > 0 && instruction.length <= 15);
    assert(instruction.text[0] != '\0');
    assert(Wh_SetFunctionHook(reinterpret_cast<void*>(&HookTestTarget), reinterpret_cast<void*>(&HookTestDetour), &g_originalAddress) == TRUE);
    assert(g_originalAddress != nullptr);
    assert(HookTestTarget(5) == 7);
    assert(Wh_ApplyHookOperations() == TRUE);
    assert(HookTestTarget(5) == 17);
    assert(Wh_RemoveFunctionHook(reinterpret_cast<void*>(&HookTestTarget)) == TRUE);
    assert(HookTestTarget(5) == 17);
    assert(Wh_ApplyHookOperations() == TRUE);
    assert(HookTestTarget(5) == 7);
    return 0;
}
