// ==WindhawkMod==
// @id com.example.windhawk-import-smoke
// @name Windhawk Import Smoke Test
// @description A small API and lifecycle compatibility build fixture.
// @version 1.0.0
// @author 00047 CI
// @include explorer.exe
// @compilerOptions -std=c++20 -lcomctl32
// ==/WindhawkMod==

// ==WindhawkModSettings==
/*
- Enabled: true
  $name: Enabled
- CaptionPrefix: "[00047] "
  $name: Caption prefix
- Mode: prefixed
  $name: Mode
  $options:
    - prefixed: Prefix the caption
    - unchanged: Leave the caption unchanged
  $hideIf: {Enabled: false}
- Opacity: 0.85
  $float: true
  $min: 0
  $max: 1
  $format: slider
  $showIf: {Enabled: true}
- Font: "Segoe UI"
  $format: fontFamily
- Shortcut: "ctrl+alt+84"
  $format: hotkey
- Rendering:
  $name: Rendering controls
  $showIf: {Enabled: true}
  - UseVisualStyles: false
  - Theme: mica
    $name: Theme
    $options:
      - mica: Mica
      - acrylic: Acrylic
    $showIf: {UseVisualStyles: true}
- Overlay: "#80FFFFFF"
  $format: colorArgb
*/
// ==/WindhawkModSettings==

// ==WindhawkModReadme==
/*
CI fixture for importing a Windhawk single-file source and compiling its
metadata, basic settings, API helpers, and lifecycle adapter.
*/
// ==/WindhawkModReadme==

#include <windhawk_api.h>

using SetWindowTextWFn = BOOL (WINAPI*)(HWND, LPCWSTR);
static SetWindowTextWFn g_originalSetWindowTextW = nullptr;

static BOOL WINAPI SmokeSetWindowTextWHook(HWND window, LPCWSTR text) {
    return g_originalSetWindowTextW ? g_originalSetWindowTextW(window, text) : FALSE;
}

BOOL Wh_ModInit() {
    if (!Wh_GetIntSetting(L"Enabled")) {
        Wh_Log(L"Smoke mod is disabled by settings.");
        return TRUE;
    }
    PCWSTR prefix = Wh_GetStringSetting(L"CaptionPrefix");
    Wh_Log(L"Imported caption prefix: %ls", prefix ? prefix : L"(null)");
    Wh_FreeStringSetting(prefix);
    if (!Wh_SetFunctionHook(
            reinterpret_cast<void*>(SetWindowTextW),
            reinterpret_cast<void*>(SmokeSetWindowTextWHook),
            reinterpret_cast<void**>(&g_originalSetWindowTextW))) {
        Wh_Log(L"Failed to register the SetWindowTextW test hook.");
        return FALSE;
    }
    return TRUE;
}
void Wh_ModAfterInit() { Wh_Log(L"After init"); }
void Wh_ModBeforeUninit() { Wh_Log(L"Before uninit"); }
void Wh_ModUninit() { Wh_Log(L"Uninit"); }
void Wh_ModSettingsChanged() { Wh_Log(L"Settings changed"); }
