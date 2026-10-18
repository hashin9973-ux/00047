#include <Windows.h>
#include "windhawk_nonhook_adapter.h"

namespace {
int g_lifecycle_step = 0;
}

BOOL Wh_ModInit(void) {
    g_lifecycle_step = 1;
    return TRUE;
}

void Wh_ModAfterInit(void) {
    if (g_lifecycle_step == 1) g_lifecycle_step = 2;
}

void Wh_ModBeforeUninit(void) {
    if (g_lifecycle_step == 2) g_lifecycle_step = 3;
}

void Wh_ModUninit(void) {
    if (g_lifecycle_step == 3) g_lifecycle_step = 4;
}

extern "C" __declspec(dllexport) int WINAPI AdapterSmokeLifecycleStep(void) {
    return g_lifecycle_step;
}

WMOD_DEFINE_WINDHAWK_LIFECYCLE(
    Wh_ModInit,
    Wh_ModAfterInit,
    Wh_ModBeforeUninit,
    Wh_ModUninit
)
