#pragma once
#ifndef WMOD_00047_WINDHAWK_NONHOOK_ADAPTER_H
#define WMOD_00047_WINDHAWK_NONHOOK_ADAPTER_H
#ifndef WMOD_BUILD_DLL
#error Define WMOD_BUILD_DLL when building a native-mod DLL before including this header.
#endif
#include "00047_mod_api.h"
#include "windhawk_hook_compat.h"
#include <atomic>
namespace wmod::windhawk_adapter {
inline void NoopCallback() noexcept {}
inline std::atomic<int>& LifecycleState() noexcept {
    static std::atomic<int> state{0};
    return state;
}
}
#define WMOD_DEFINE_WINDHAWK_LIFECYCLE(init_callback, after_init_callback, before_uninit_callback, uninit_callback) \
    WMOD_00047_EXPORT std::uint32_t WINAPI WmodGetApiVersion(void) { \
        return WMOD_00047_ABI_VERSION; \
    } \
    WMOD_00047_EXPORT BOOL WINAPI WmodInitializeV1(void) { \
        int expected = 0; \
        auto& state = ::wmod::windhawk_adapter::LifecycleState(); \
        if (!state.compare_exchange_strong(expected, 1)) return expected == 2 ? TRUE : FALSE; \
        if (!::wmod::windhawk_hook_compat::BeginHookOperations()) { state.store(0); return FALSE; } \
        bool init_succeeded = false; \
        try { \
            if (!(init_callback)()) { \
                ::wmod::windhawk_hook_compat::ShutdownHookOperations(); \
                state.store(0); \
                return FALSE; \
            } \
            init_succeeded = true; \
            if (!::wmod::windhawk_hook_compat::ApplyHookOperations()) { \
                try { (before_uninit_callback)(); } catch (...) {} \
                ::wmod::windhawk_hook_compat::ShutdownHookOperations(); \
                try { (uninit_callback)(); } catch (...) {} \
                state.store(0); \
                return FALSE; \
            } \
            (after_init_callback)(); \
            state.store(2); \
            return TRUE; \
        } catch (...) { \
            if (init_succeeded) { \
                try { (before_uninit_callback)(); } catch (...) {} \
                ::wmod::windhawk_hook_compat::ShutdownHookOperations(); \
                try { (uninit_callback)(); } catch (...) {} \
            } else { \
                ::wmod::windhawk_hook_compat::ShutdownHookOperations(); \
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
        ::wmod::windhawk_hook_compat::ShutdownHookOperations(); \
        try { (uninit_callback)(); } catch (...) {} \
        state.store(0); \
    }
#endif // WMOD_00047_WINDHAWK_NONHOOK_ADAPTER_H
