#include "00047_mod_api.h"

// CI fixture proving that a plain C DLL can implement the published lifecycle ABI.
static volatile LONG g_initialized = 0;

WMOD_00047_EXPORT uint32_t WINAPI WmodGetApiVersion(void) {
    return WMOD_00047_ABI_VERSION;
}

WMOD_00047_EXPORT BOOL WINAPI WmodInitializeV1(void) {
    InterlockedExchange(&g_initialized, 1);
    return TRUE;
}

WMOD_00047_EXPORT void WINAPI WmodShutdownV1(void) {
    InterlockedExchange(&g_initialized, 0);
}

WMOD_00047_EXPORT BOOL WINAPI WmodIsInitializedV1(void) {
    return InterlockedCompareExchange(&g_initialized, 0, 0) != 0 ? TRUE : FALSE;
}

WMOD_00047_EXPORT BOOL WINAPI WmodCanUnloadV1(void) {
    return InterlockedCompareExchange(&g_initialized, 0, 0) == 0 ? TRUE : FALSE;
}
