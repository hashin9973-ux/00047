#pragma once
#ifndef WMOD_00047_MOD_API_H
#define WMOD_00047_MOD_API_H

#include <Windows.h>
#include <cstdint>

// 00047 native mod ABI v1.
// A module must export all five lifecycle functions/queries below with these exact names.
// The host maps an x64 DLL into an allow-listed shell process, validates the ABI
// version, calls WmodInitializeV1, and calls WmodShutdownV1 before unloading.
// Native mods execute in-process and are not sandboxed.

#define WMOD_00047_ABI_VERSION 1u

#ifdef WMOD_BUILD_DLL
#define WMOD_00047_EXPORT extern "C" __declspec(dllexport)
#else
#define WMOD_00047_EXPORT extern "C" __declspec(dllimport)
#endif

WMOD_00047_EXPORT std::uint32_t WINAPI WmodGetApiVersion(void);
WMOD_00047_EXPORT BOOL WINAPI WmodInitializeV1(void);
WMOD_00047_EXPORT void WINAPI WmodShutdownV1(void);
// The host uses these lifecycle queries to safely retry enable and unload operations.
WMOD_00047_EXPORT BOOL WINAPI WmodIsInitializedV1(void);
WMOD_00047_EXPORT BOOL WINAPI WmodCanUnloadV1(void);

#endif // WMOD_00047_MOD_API_H
