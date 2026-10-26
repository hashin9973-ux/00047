#pragma once
#ifndef WMOD_00047_MOD_API_H
#define WMOD_00047_MOD_API_H

#include <Windows.h>
#include <stdint.h>
#ifdef __cplusplus
#include <cstdint> // Preserve std::uint32_t for existing C++ mods.
#endif

// 00047 native mod ABI v2.
// A module must export all five lifecycle functions/queries below with these exact names.
// The host maps an x64 DLL into an allow-listed shell process, validates the ABI
// version, calls WmodInitializeV1, and calls WmodShutdownV1 before unloading.
// Native mods execute in-process and are not sandboxed.

#define WMOD_00047_ABI_VERSION 2u

// Preserve explicit C linkage on every C++ declaration/definition while keeping
// the same header valid for plain C translation units.
#ifdef __cplusplus
#ifdef WMOD_BUILD_DLL
#define WMOD_00047_EXPORT extern "C" __declspec(dllexport)
#else
#define WMOD_00047_EXPORT extern "C" __declspec(dllimport)
#endif
#else
#ifdef WMOD_BUILD_DLL
#define WMOD_00047_EXPORT __declspec(dllexport)
#else
#define WMOD_00047_EXPORT __declspec(dllimport)
#endif
#endif

WMOD_00047_EXPORT uint32_t WINAPI WmodGetApiVersion(void);
WMOD_00047_EXPORT BOOL WINAPI WmodInitializeV1(void);
WMOD_00047_EXPORT void WINAPI WmodShutdownV1(void);
// The host uses these lifecycle queries to safely retry enable and unload operations.
WMOD_00047_EXPORT BOOL WINAPI WmodIsInitializedV1(void);
WMOD_00047_EXPORT BOOL WINAPI WmodCanUnloadV1(void);

#endif // WMOD_00047_MOD_API_H
