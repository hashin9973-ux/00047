#pragma once
#ifndef WMOD_00047_WINDHAWK_NONHOOK_ADAPTER_H
#define WMOD_00047_WINDHAWK_NONHOOK_ADAPTER_H

// Opt-in lifecycle adapter for non-hook Windhawk mods. This maps callback
// functions onto the 00047 native-mod ABI only; it does not implement Windhawk
// hooks, symbols, disassembly, or settings-change notifications.
#ifndef WMOD_BUILD_DLL
#error Define WMOD_BUILD_DLL when building a native-mod DLL before including this header.
#endif

#include "00047_mod_api.h"

namespace wmod::windhawk_adapter {
inline void NoopCallback() noexcept {}
}

// Call after the callback definitions in one translation unit.
// Optional void callbacks can use wmod::windhawk_adapter::NoopCallback.
// Use only when the mod's settings/storage APIs have been ported to the
// non-hook compatibility header and the mod does not require unsupported hooks.
#define WMOD_DEFINE_WINDHAWK_LIFECYCLE(init_callback, after_init_callback, before_uninit_callback, uninit_callback) \
    WMOD_00047_EXPORT std::uint32_t WINAPI WmodGetApiVersion(void) { \
        return WMOD_00047_ABI_VERSION; \
    } \
    WMOD_00047_EXPORT BOOL WINAPI WmodInitializeV1(void) { \
        if (!(init_callback)()) return FALSE; \
        (after_init_callback)(); \
        return TRUE; \
    } \
    WMOD_00047_EXPORT void WINAPI WmodShutdownV1(void) { \
        (before_uninit_callback)(); \
        (uninit_callback)(); \
    }

#endif // WMOD_00047_WINDHAWK_NONHOOK_ADAPTER_H
