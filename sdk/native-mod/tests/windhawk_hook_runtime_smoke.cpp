#include "windhawk_hook_compat.h"

#include <Windows.h>
#include <atomic>

using TestFunction = int(__cdecl*)(int);
static void* g_originalAddress = nullptr;
static std::atomic<bool> g_holdDetour{false};
static std::atomic<bool> g_insideDetour{false};
static HANDLE g_persistentWorkerStopEvent = nullptr;
static HANDLE g_persistentWorkerThread = nullptr;

static DWORD WINAPI PersistentWorkerThreadProc(LPVOID parameter) noexcept {
    const HANDLE stopEvent = static_cast<HANDLE>(parameter);
    // Kept running but idle inside a Windows wait to model a mod-owned worker
    // whose current IP alone may not indicate the DLL that owns its entrypoint.
    while (WaitForSingleObject(stopEvent, 100) == WAIT_TIMEOUT) {
    }
    return 0;
}

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
        return 123;
    }
    auto original = reinterpret_cast<TestFunction>(g_originalAddress);
    return original(value) + 10;
}

extern "C" __declspec(dllexport) int WINAPI HookRuntimeTarget(int value) {
    return HookTestTarget(value);
}

extern "C" __declspec(dllexport) void* WINAPI HookRuntimeTargetAddress(void) {
    return reinterpret_cast<void*>(&HookTestTarget);
}

extern "C" __declspec(dllexport) BOOL WINAPI HookRuntimeExternalBegin(void) {
    return wmod::windhawk_hook_compat::BeginHookOperations();
}

extern "C" __declspec(dllexport) BOOL WINAPI HookRuntimeExternalInstall(
    void* target, void** original) {
    const BOOL result = Wh_SetFunctionHook(
        target, reinterpret_cast<void*>(&HookTestDetour), &g_originalAddress);
    if (original) *original = result ? g_originalAddress : nullptr;
    return result;
}

extern "C" __declspec(dllexport) BOOL WINAPI HookRuntimeExternalApply(void) {
    return Wh_ApplyHookOperations();
}

extern "C" __declspec(dllexport) BOOL WINAPI HookRuntimeExternalRemove(void* target) {
    return Wh_RemoveFunctionHook(target);
}

extern "C" __declspec(dllexport) void WINAPI HookRuntimeExternalShutdown(void) {
    wmod::windhawk_hook_compat::ShutdownHookOperations();
}

extern "C" __declspec(dllexport) BOOL WINAPI HookRuntimeExternalCanUnload(void) {
    return wmod::windhawk_hook_compat::CanUnload();
}

extern "C" __declspec(dllexport) BOOL WINAPI HookRuntimeDisasm(WH_DISASM_RESULT* result) {
    return Wh_Disasm(reinterpret_cast<void*>(&HookTestTarget), result);
}

extern "C" __declspec(dllexport) BOOL WINAPI HookRuntimeDisasmAddress(
    void* address, WH_DISASM_RESULT* result) {
    return Wh_Disasm(address, result);
}

extern "C" __declspec(dllexport) BOOL WINAPI HookRuntimeBegin(void) {
    return wmod::windhawk_hook_compat::BeginHookOperations();
}

extern "C" __declspec(dllexport) BOOL WINAPI HookRuntimeInstall(void) {
    return Wh_SetFunctionHook(
        reinterpret_cast<void*>(&HookTestTarget),
        reinterpret_cast<void*>(&HookTestDetour),
        &g_originalAddress);
}

extern "C" __declspec(dllexport) BOOL WINAPI HookRuntimeApply(void) {
    return Wh_ApplyHookOperations();
}

extern "C" __declspec(dllexport) BOOL WINAPI HookRuntimeRemove(void) {
    return Wh_RemoveFunctionHook(reinterpret_cast<void*>(&HookTestTarget));
}

extern "C" __declspec(dllexport) void WINAPI HookRuntimeShutdown(void) {
    wmod::windhawk_hook_compat::ShutdownHookOperations();
}

extern "C" __declspec(dllexport) BOOL WINAPI HookRuntimeCanUnload(void) {
    return wmod::windhawk_hook_compat::CanUnload();
}

extern "C" __declspec(dllexport) void WINAPI HookRuntimeHoldDetour(BOOL hold) {
    g_holdDetour.store(hold != FALSE, std::memory_order_release);
}

extern "C" __declspec(dllexport) BOOL WINAPI HookRuntimeInsideDetour(void) {
    return g_insideDetour.load(std::memory_order_acquire) ? TRUE : FALSE;
}

extern "C" __declspec(dllexport) BOOL WINAPI HookRuntimeStartPersistentWorker(void) {
    if (g_persistentWorkerThread || g_persistentWorkerStopEvent) return FALSE;
    HANDLE stopEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (!stopEvent) return FALSE;
    HANDLE thread = CreateThread(nullptr, 0, &PersistentWorkerThreadProc, stopEvent, 0, nullptr);
    if (!thread) {
        CloseHandle(stopEvent);
        return FALSE;
    }
    g_persistentWorkerStopEvent = stopEvent;
    g_persistentWorkerThread = thread;
    return TRUE;
}

extern "C" __declspec(dllexport) BOOL WINAPI HookRuntimeStopPersistentWorker(void) {
    if (!g_persistentWorkerThread || !g_persistentWorkerStopEvent) return FALSE;
    if (!SetEvent(g_persistentWorkerStopEvent)) return FALSE;
    const DWORD wait = WaitForSingleObject(g_persistentWorkerThread, 5000);
    if (wait != WAIT_OBJECT_0) return FALSE;
    CloseHandle(g_persistentWorkerThread);
    CloseHandle(g_persistentWorkerStopEvent);
    g_persistentWorkerThread = nullptr;
    g_persistentWorkerStopEvent = nullptr;
    return TRUE;
}
