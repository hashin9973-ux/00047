#include <Windows.h>
#include <atomic>
#include <fstream>
#include "windhawk_settings_changed_adapter.h"

namespace {
std::atomic<unsigned long> g_settings_changed_count{0};
std::atomic<bool> g_block_callback{false};
std::atomic<bool> g_callback_entered{false};

BOOL Wh_ModInit(void) {
    return TRUE;
}

void Wh_ModAfterInit(void) {}

BOOL Wh_ModSettingsChanged(BOOL* bReload) {
    g_callback_entered.store(true, std::memory_order_release);
    while (g_block_callback.load(std::memory_order_acquire)) Sleep(1);
    if (!bReload) return FALSE;
    *bReload = FALSE;
    const unsigned long previous =
        g_settings_changed_count.fetch_add(1, std::memory_order_relaxed);
    if (previous == 0) {
        // Exercise the case where settings change while the callback itself is
        // running. The watcher must already have re-armed its directory request.
        const auto path = wmod::SettingsPath();
        if (!path.empty()) {
            std::ofstream file(path, std::ios::binary | std::ios::trunc);
            file << R"json({"intSetting":103})json";
        }
    } else if (previous == 1) {
        *bReload = TRUE;
    }
    return TRUE;
}

void Wh_ModBeforeUninit(void) {}

void Wh_ModUninit(void) {}
}

extern "C" __declspec(dllexport) unsigned long WINAPI AdapterSettingsChangedCount(void) {
    return g_settings_changed_count.load(std::memory_order_relaxed);
}

extern "C" __declspec(dllexport) void WINAPI AdapterSettingsChangedBlock(BOOL block) {
    if (block) g_callback_entered.store(false, std::memory_order_release);
    g_block_callback.store(block != FALSE, std::memory_order_release);
}

extern "C" __declspec(dllexport) BOOL WINAPI AdapterSettingsChangedCallbackEntered(void) {
    return g_callback_entered.load(std::memory_order_acquire) ? TRUE : FALSE;
}

WMOD_DEFINE_WINDHAWK_LIFECYCLE_WITH_SETTINGS_CHANGED(
    Wh_ModInit,
    Wh_ModAfterInit,
    Wh_ModSettingsChanged,
    Wh_ModBeforeUninit,
    Wh_ModUninit
)
