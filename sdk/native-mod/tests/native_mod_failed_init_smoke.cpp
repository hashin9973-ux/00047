#include "00047_mod_api.h"

// Test-only DLL that passes ABI validation and then rejects initialization,
// forcing the real remote loader to execute shutdown, quiescence, and rollback.
WMOD_00047_EXPORT std::uint32_t WINAPI WmodGetApiVersion(void) {
    return WMOD_00047_ABI_VERSION;
}
WMOD_00047_EXPORT BOOL WINAPI WmodInitializeV1(void) {
    return FALSE;
}
WMOD_00047_EXPORT void WINAPI WmodShutdownV1(void) {}
WMOD_00047_EXPORT BOOL WINAPI WmodIsInitializedV1(void) {
    return FALSE;
}
WMOD_00047_EXPORT BOOL WINAPI WmodCanUnloadV1(void) {
    return TRUE;
}
