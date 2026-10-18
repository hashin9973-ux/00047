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
#include <atomic>

namespace wmod::windhawk_adapter {
inline void NoopCallback() noexcept {}

// Internal adapter state: 0 = stopped, 1 = initializing, 2 = running,
// 3 = shutting down. It prevents duplicate initialization/shutdown callback
// execution when the host retries a lifecycle operation.
inline std::atomic<int>& LifecycleState() noexcept {
    static std::atomic<int> state{0};
    return state;
}
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
        int expected = 0; \
        auto& state = ::wmod::windhawk_adapter::LifecycleState(); \
        if (!state.compare_exchange_strong(expected, 1)) return expected == 2 ? TRUE : FALSE; \
        bool init_succeeded = false; \
        try { \
            if (!(init_callback)()) { state.store(0); return FALSE; } \
            init_succeeded = true; \
            (after_init_callback)(); \
            state.store(2); \
            return TRUE; \
        } catch (...) { \
            if (init_succeeded) { \
                try { (before_uninit_callback)(); } catch (...) {} \
                try { (uninit_callback)(); } catch (...) {} \
            } \
            state.store(0); \
            return FALSE; \
        } \
    } \
    WMOD_00047_EXPORT void WINAPI WmodShutdownV1(void) { \
        int expected = 2; \
        auto& state = ::wmod::windhawk_adapter::LifecycleState(); \
        if (!state.compare_exchange_strong(expected, 3)) return; \
        try { (before_uninit_callback)(); } catch (...) {} \
        try { (uninit_callback)(); } catch (...) {} \
        state.store(0); \
    }

#endif // WMOD_00047_WINDHAWK_NONHOOK_ADAPTER_H
