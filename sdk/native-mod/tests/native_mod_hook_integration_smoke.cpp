#include "00047_mod_api.h"
#include "windhawk_hook_compat.h"

#include <Windows.h>

namespace {
using RemoteTargetFn = int (WINAPI*)();
void* g_originalTarget = nullptr;
bool g_initialized = false;

int WINAPI RemoteTargetDetour(void) {
    const auto original = reinterpret_cast<RemoteTargetFn>(g_originalTarget);
    return original ? original() + 10 : -1;
}
} // namespace

WMOD_00047_EXPORT std::uint32_t WINAPI WmodGetApiVersion(void) {
    return WMOD_00047_ABI_VERSION;
}

WMOD_00047_EXPORT BOOL WINAPI WmodInitializeV1(void) {
    if (g_initialized) return TRUE;
    if (!wmod::windhawk_hook_compat::BeginHookOperations()) return FALSE;

    HMODULE targetModule = GetModuleHandleW(nullptr);
    auto target = targetModule
        ? GetProcAddress(targetModule, "RemoteTargetFunction")
        : nullptr;
    if (!target ||
        !Wh_SetFunctionHook(
            reinterpret_cast<void*>(target),
            reinterpret_cast<void*>(&RemoteTargetDetour),
            &g_originalTarget) ||
        !Wh_ApplyHookOperations()) {
        wmod::windhawk_hook_compat::ShutdownHookOperations();
        g_originalTarget = nullptr;
        return FALSE;
    }

    g_initialized = true;
    return TRUE;
}

WMOD_00047_EXPORT void WINAPI WmodShutdownV1(void) {
    wmod::windhawk_hook_compat::ShutdownHookOperations();
    g_initialized = false;
    g_originalTarget = nullptr;
}

WMOD_00047_EXPORT BOOL WINAPI WmodIsInitializedV1(void) {
    return g_initialized ? TRUE : FALSE;
}

WMOD_00047_EXPORT BOOL WINAPI WmodCanUnloadV1(void) {
    return wmod::windhawk_hook_compat::CanUnload();
}
