# Native blur source notice

This component vendors and adapts the native Direct2D/Composition blur implementation referenced by the Windhawk **Windows 11 File Explorer Styler** source:
https://github.com/ramensoftware/windhawk-mods/blob/main/mods/windows-11-file-explorer-styler.wh.cpp

Windhawk's source comments that the blur background implementation is copied from TranslucentTB. The upstream source for the underlying XAML blur brush is:
https://github.com/TranslucentTB/TranslucentTB/blob/release/ExplorerTAP/XamlBlurBrush.cpp

The reusable Direct2D effect interop classes, procedural noise stream, and Windhawk `XamlBlurBrush::CreateEffectBrush` effect graph are included here. The XAML-only brush lifecycle wrapper is replaced by a native Desktop Composition host, because 00047 uses Tauri/WebView2 and does not have a WinUI/XAML visual tree.

Both upstream projects publish this implementation under GNU GPL version 3.0. The license text is in `COPYING.GPL-3.0`. The copied component is marked GPL-3.0-only; review the licensing implications before distributing 00047 with this component.
