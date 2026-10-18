#include <Windows.h>
#include <atomic>
#include <fstream>
#include "windhawk_settings_changed_adapter.h"

namespace {
std::atomic<unsigned long> g_settings_changed_count{0};

BOOL Wh_ModInit(void) {
    return TRUE;
}

void Wh_ModAfterInit(void) {}

void Wh_ModSettingsChanged(void) {
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
    }
}

void Wh_ModBeforeUninit(void) {}

void Wh_ModUninit(void) {}
}

extern "C" __declspec(dllexport) unsigned long WINAPI AdapterSettingsChangedCount(void) {
    return g_settings_changed_count.load(std::memory_order_relaxed);
}

WMOD_DEFINE_WINDHAWK_LIFECYCLE_WITH_SETTINGS_CHANGED(
    Wh_ModInit,
    Wh_ModAfterInit,
    Wh_ModSettingsChanged,
    Wh_ModBeforeUninit,
    Wh_ModUninit
)
