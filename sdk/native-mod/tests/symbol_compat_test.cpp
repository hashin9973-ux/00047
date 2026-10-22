#include <Windows.h>
#include <iostream>
#include "windhawk_nonhook_compat.h"

#if defined(_MSC_VER)
#pragma optimize("", off)
#endif
extern "C" __declspec(noinline) int CompatibilityProbeSymbol() {
    return 47;
}
#if defined(_MSC_VER)
#pragma optimize("", on)
#endif

static void* g_originalProbe = nullptr;
extern "C" __declspec(noinline) int CompatibilityProbeDetour() {
    auto original = reinterpret_cast<int(__cdecl*)()>(g_originalProbe);
    return original() + 10;
}

#define CHECK(condition) do { \
    if (!(condition)) { \
        std::wcerr << L"FAILED check at line " << __LINE__ << L"\n"; \
        return 1; \
    } \
} while (false)

int wmain() {
    CHECK(CompatibilityProbeSymbol() == 47);

    WH_FIND_SYMBOL first{};
    WH_FIND_SYMBOL_OPTIONS invalidOptions{};
    invalidOptions.optionsSize = 0;
    invalidOptions.symbolServer = L"";
    CHECK(Wh_FindFirstSymbol(GetModuleHandleW(nullptr), &invalidOptions, &first) == nullptr);
    CHECK(first.address == nullptr && first.symbol == nullptr && first.symbolDecorated == nullptr);

    WH_FIND_SYMBOL_OPTIONS options{};
    options.optionsSize = sizeof(options);
    // Disable network access in this test. The target executable is built with
    // a local PDB so the test does not depend on the Microsoft symbol server.
    options.symbolServer = L"";
    options.noUndecoratedSymbols = FALSE;

    HANDLE search = Wh_FindFirstSymbol(GetModuleHandleW(nullptr), &options, &first);
    CHECK(search != nullptr);
    bool foundProbe = false;
    WH_FIND_SYMBOL current = first;
    do {
        if (current.address == reinterpret_cast<void*>(&CompatibilityProbeSymbol) ||
            (current.symbol && std::wcscmp(current.symbol, L"CompatibilityProbeSymbol") == 0) ||
            (current.symbolDecorated &&
             std::wcscmp(current.symbolDecorated, L"CompatibilityProbeSymbol") == 0)) {
            foundProbe = true;
            break;
        }
    } while (Wh_FindNextSymbol(search, &current));
    Wh_FindCloseSymbol(search);
    CHECK(foundProbe);

    // Decorated-only enumeration still returns a decorated name and address.
    options.noUndecoratedSymbols = TRUE;
    search = Wh_FindFirstSymbol(GetModuleHandleW(nullptr), &options, &first);
    CHECK(search != nullptr);
    CHECK(first.address != nullptr);
    CHECK(first.symbolDecorated != nullptr);
    CHECK(first.symbol == nullptr);
    Wh_FindCloseSymbol(search);

    // Resolve a symbol name, queue its hook, apply it and restore the target.
    struct ProbeSelector {
        PCWSTR string;
        size_t length;
    } probeSelector{L"CompatibilityProbeSymbol", wcslen(L"CompatibilityProbeSymbol")};
    void* originalProbe = nullptr;
    WH_SYMBOL_HOOK probeHook{};
    probeHook.symbols = reinterpret_cast<const decltype(probeHook.symbols)>(&probeSelector);
    probeHook.symbolsCount = 1;
    probeHook.pOriginalFunction = &originalProbe;
    probeHook.hookFunction = reinterpret_cast<void*>(&CompatibilityProbeDetour);
    probeHook.optional = false;

    WH_HOOK_SYMBOLS_OPTIONS hookOptions{};
    hookOptions.optionsSize = sizeof(hookOptions);
    hookOptions.symbolServer = L"";
    hookOptions.noUndecoratedSymbols = FALSE;
    CHECK(Wh_HookSymbols(GetModuleHandleW(nullptr), &probeHook, 1, &hookOptions) == TRUE);
    CHECK(originalProbe != nullptr);
    g_originalProbe = originalProbe;
    CHECK(CompatibilityProbeSymbol() == 47);
    CHECK(Wh_ApplyHookOperations() == TRUE);
    CHECK(CompatibilityProbeSymbol() == 57);
    CHECK(Wh_RemoveFunctionHook(reinterpret_cast<void*>(&CompatibilityProbeSymbol)) == TRUE);
    CHECK(Wh_ApplyHookOperations() == TRUE);
    CHECK(CompatibilityProbeSymbol() == 47);

    // NULL handles are explicitly safe no-ops.
    CHECK(Wh_FindNextSymbol(nullptr, &first) == FALSE);
    Wh_FindCloseSymbol(nullptr);
    std::wcout << L"DbgHelp symbol compatibility tests passed.\n";
    return 0;
}
