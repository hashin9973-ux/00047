#pragma once
#ifndef WMOD_00047_WINDHAWK_HOOK_COMPAT_H
#define WMOD_00047_WINDHAWK_HOOK_COMPAT_H

#include <Windows.h>
#include <cstddef>

#ifndef WMOD_WINDHAWK_DISASM_TYPES
#define WMOD_WINDHAWK_DISASM_TYPES
typedef struct tagWH_DISASM_RESULT {
    size_t length;
    char text[96];
} WH_DISASM_RESULT;
#endif

// Source compatibility for Windhawk's x64 function-hook API. These functions
// are implemented by windhawk_hook_compat.cpp and must be linked into the mod
// DLL together with the vendored MinHook sources.
BOOL Wh_SetFunctionHook(void* targetFunction, void* hookFunction, void** originalFunction);
BOOL Wh_RemoveFunctionHook(void* targetFunction);
BOOL Wh_ApplyHookOperations(void);
BOOL Wh_Disasm(void* address, WH_DISASM_RESULT* result);

namespace wmod::windhawk_hook_compat {
BOOL BeginHookOperations() noexcept;
BOOL ApplyHookOperations() noexcept;
BOOL CanUnload() noexcept;
void ShutdownHookOperations() noexcept;
}

#endif // WMOD_00047_WINDHAWK_HOOK_COMPAT_H
