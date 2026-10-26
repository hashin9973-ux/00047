#pragma once
#ifndef WMOD_00047_WINDHAWK_API_H
#define WMOD_00047_WINDHAWK_API_H

// Conventional source include name used by Windhawk mods.
// This exposes 00047's supported source-level Wh_* helpers, hook declarations,
// and symbol helpers. It is not a binary-compatible Windhawk runtime.
#ifndef __cplusplus
#error The Windhawk API compatibility layer is currently C++-only.
#endif

#include <Windows.h>
#include "windhawk_nonhook_compat.h"

#endif // WMOD_00047_WINDHAWK_API_H
