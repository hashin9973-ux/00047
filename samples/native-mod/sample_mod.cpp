#include "00047_mod_api.h"
#include "00047_mod_services.h"

static bool g_initialized = false;

std::uint32_t WINAPI WmodGetApiVersion(void) {
    return WMOD_00047_ABI_VERSION;
}

BOOL WINAPI WmodInitializeV1(void) {
    if (g_initialized) {
        return TRUE;
    }

    OutputDebugStringW(L"[00047 sample native mod] WmodInitializeV1 entered Explorer.\n");
    wmod::Log(L"info", L"WmodInitializeV1 entered the target process.");
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
}
