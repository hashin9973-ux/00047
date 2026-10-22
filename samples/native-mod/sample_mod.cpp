#include "00047_mod_api.h"
#include "00047_mod_services.h"
#include "windhawk_hook_compat.h"

static bool g_initialized = false;

std::uint32_t WINAPI WmodGetApiVersion(void) {
    return WMOD_00047_ABI_VERSION;
}

BOOL WINAPI WmodInitializeV1(void) {
    if (g_initialized) {
        return TRUE;
    }
    if (!wmod::windhawk_hook_compat::BeginHookOperations()) return FALSE;

    OutputDebugStringW(L"[00047 sample native mod] WmodInitializeV1 entered Explorer.\n");
    wmod::Log(L"info", L"WmodInitializeV1 entered the target process.");
    // Demonstrate safe export lookup without installing a hook or changing behavior.
    const auto get_current_process_id = wmod::ResolveSymbol(L"kernel32.dll", "GetCurrentProcessId");
    wmod::Log(L"info", get_current_process_id
        ? L"SDK symbol resolver found kernel32!GetCurrentProcessId."
        : L"SDK symbol resolver could not find kernel32!GetCurrentProcessId.");
    g_initialized = true;
    return TRUE;
}

void WINAPI WmodShutdownV1(void) {
    if (!g_initialized) {
        return;
    }

    OutputDebugStringW(L"[00047 sample native mod] WmodShutdownV1 called before unload.\n");
    wmod::Log(L"info", L"WmodShutdownV1 called before unload.");
    g_initialized = false;
    wmod::windhawk_hook_compat::ShutdownHookOperations();
}

BOOL WINAPI WmodIsInitializedV1(void) {
    return g_initialized ? TRUE : FALSE;
}

BOOL WINAPI WmodCanUnloadV1(void) {
    return wmod::windhawk_hook_compat::CanUnload();
}
