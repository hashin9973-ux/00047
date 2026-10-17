# Native blur source notice

This file vendors and adapts the native Direct2D/Composition effect implementation from the Windhawk **Windows 11 File Explorer Styler** source:
https://github.com/ramensoftware/windhawk-mods/blob/main/mods/windows-11-file-explorer-styler.wh.cpp

The Windhawk source states it is published under the GNU General Public License version 3.0. The reusable Direct2D effect classes, procedural noise stream, and `XamlBlurBrush::CreateEffectBrush` effect graph are included here. The XAML-only brush lifecycle wrapper is replaced by a native Desktop Composition host, because 00047 uses Tauri/WebView2 and does not have a WinUI/XAML visual tree.

License text is in `COPYING.GPL-3.0`. The copied component is marked GPL-3.0-only; review the licensing implications before distributing 00047 with this component.
