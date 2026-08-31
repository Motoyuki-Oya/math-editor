---
name: testing-planetext-windows
description: Run and efficiently GUI-test the Planetext Tauri/WebView2 application on Windows using HWND-based window and native file-dialog automation, including RTL, IME, structures, screenshots, and rebuild checks.
triggers:
  - user
  - model
---

# Testing Planetext on Windows

Use this skill for Windows GUI/WebView validation of Planetext. Unit tests do not replace these checks for DOM layout, caret movement, BiDi, IME, native dialogs, scrolling, or structure rendering.

## Before starting the app

- Before running `cargo tauri dev`, tell the user explicitly. The user may interact with the window otherwise.
- If the user already started it, reuse that process. Do not start a second server or application.
- Check the exact process rather than guessing from a title:

```powershell
Get-Process planetext | Select-Object Id, MainWindowTitle, MainWindowHandle, Responding
```

- A successful frontend rebuild must contain `Compiling planetext-ui`, `applying new distribution`, and `success`. A fast Tauri-only build does not prove that the WASM changed.
- When needed, compare the source and `dist` timestamps. Avoid running `trunk build` concurrently with the watcher's build because both can race while writing the staging/dist files.

## Always reacquire the window rectangle

The Planetext window can move between monitors and DPI scales during testing. Never reuse hard-coded coordinates from an earlier step.

```powershell
Add-Type @'
using System;
using System.Runtime.InteropServices;
public class PlanetextWindow {
  [DllImport("user32.dll")]
  public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [StructLayout(LayoutKind.Sequential)]
  public struct RECT { public int L, T, R, B; }
}
'@

$p = Get-Process planetext
$r = New-Object PlanetextWindow+RECT
[PlanetextWindow]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
"$($r.L),$($r.T),$($r.R),$($r.B)"
```

Use `$r.L + relativeX` and `$r.T + relativeY` for clicks. Reacquire the rectangle after dialogs, reloads, monitor changes, or window activation.

## Open a test file through the native dialog

Do not type a full path with plain `SendKeys`. If the native dialog is not actually focused, the path is inserted into the editor document. Address-bar accelerators are also unreliable in the Japanese Windows dialog.

Open the dialog, then set its filename edit control through HWND:

```powershell
$ws = New-Object -ComObject WScript.Shell
$ws.AppActivate('Planetext') | Out-Null
$ws.SendKeys('^o')
Start-Sleep -Seconds 2

Add-Type @'
using System;
using System.Runtime.InteropServices;
public class PlanetextDialog {
  [DllImport("user32.dll", CharSet=CharSet.Auto)]
  public static extern IntPtr FindWindow(string className, string title);
  [DllImport("user32.dll")]
  public static extern IntPtr GetDlgItem(IntPtr window, int id);
  [DllImport("user32.dll")]
  public static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string className, string title);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)]
  public static extern IntPtr SendMessage(IntPtr window, uint message, IntPtr wParam, string value);
  [DllImport("user32.dll")]
  public static extern bool SetForegroundWindow(IntPtr window);
}
'@

$dialog = [PlanetextDialog]::FindWindow('#32770', '開く')
if ($dialog -eq [IntPtr]::Zero) { throw 'Open dialog not found' }
$host = [PlanetextDialog]::GetDlgItem($dialog, 1148)
$combo = [PlanetextDialog]::FindWindowEx($host, [IntPtr]::Zero, 'ComboBox', $null)
$edit = [PlanetextDialog]::FindWindowEx($combo, [IntPtr]::Zero, 'Edit', $null)
if ($edit -eq [IntPtr]::Zero) { throw 'Filename edit control not found' }
[PlanetextDialog]::SendMessage($edit, 0x000C, [IntPtr]::Zero, 'C:\absolute\test-file.txt') | Out-Null
[PlanetextDialog]::SetForegroundWindow($dialog) | Out-Null
$ws.SendKeys('{ENTER}')
Start-Sleep -Seconds 2
```

`0x000C` is `WM_SETTEXT`. Use a unique temporary filename for each scenario so restored drafts or already-open document handles do not contaminate the result.

For Save As, find the `#32770` window whose title is `名前を付けて保存` and use the same control chain. Verify the resulting file with the `read` tool, not only by looking at the editor.

## Activate and click the actual application

- `WScript.Shell.AppActivate('Planetext')` may be used only after checking the `planetext` process and closing/canceling any native dialog.
- Verify that no `#32770` open/save dialog remains before sending editor keys.
- Use `SetCursorPos` relative to the current window rectangle:

```powershell
Add-Type @'
using System;
using System.Runtime.InteropServices;
public class PlanetextMouse {
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, uint extraInfo);
}
'@

[PlanetextMouse]::SetCursorPos($r.L + 400, $r.T + 100) | Out-Null
[PlanetextMouse]::mouse_event(2, 0, 0, 0, 0)
[PlanetextMouse]::mouse_event(4, 0, 0, 0, 0)
```

- Clicking a structure can hit its outer node instead of the intended slot. Prefer deterministic keyboard entry: click the document, press `Home`, then `Right` to enter the first structure slot.
- Confirm a caret by its editing effect, not only its blinking line. Insert a distinctive character and inspect the resulting structure or saved notation.

## Capture the Planetext window

```powershell
Add-Type -AssemblyName System.Drawing
$p = Get-Process planetext
$r = New-Object PlanetextWindow+RECT
[PlanetextWindow]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
$bitmap = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.CopyFromScreen($r.L, $r.T, 0, 0, $bitmap.Size)
$output = Join-Path $env:TEMP 'planetext-check.png'
$bitmap.Save($output)
$graphics.Dispose()
$bitmap.Dispose()
$output
```

Read the PNG with the `read` tool. If the capture shows another application, reacquire the Planetext rectangle and foreground window; do not infer product behavior from that image.

## Japanese IME test

Windows Japanese IME can be toggled with `VK_KANJI` (`0x19`) when only the Japanese input language is installed:

```powershell
Add-Type @'
using System;
using System.Runtime.InteropServices;
public class PlanetextKeyboard {
  [DllImport("user32.dll")]
  public static extern void keybd_event(byte key, byte scan, uint flags, UIntPtr extraInfo);
}
'@
[PlanetextKeyboard]::keybd_event(0x19, 0, 0, [UIntPtr]::Zero)
[PlanetextKeyboard]::keybd_event(0x19, 0, 2, [UIntPtr]::Zero)
```

Recommended structure scenario:

1. Open a file containing `$(√ )`.
2. Focus the document, press `Home`, then `Right`; verify a pasted `X` appears inside the root before testing IME.
3. Toggle Japanese IME and type `nihongo`.
4. Verify `にほんご` appears inline inside the root and the native candidate window is anchored near it.
5. Press Enter once to commit the IME candidate.
6. Verify the committed text remains inside the root and no document line is created.
7. Press Enter again to leave the structure.
8. Paste a known character using the clipboard and verify it appears outside the root. This proves input did not freeze.

Do not use `KeyboardEvent.isComposing()` alone to decide whether an Enter belongs to the editor. WebView2 can report false for the IME commit Enter while composition is still active.

## RTL and mixed BiDi test

- Put Arabic with combining marks and an English run in a file, for example `السَّلَام ABC عليكم`.
- Verify the first strong character makes the document line RTL.
- At the visual right edge, ArrowRight must stay put; ArrowLeft must move visually left.
- Inside the English run, ArrowRight must move visually right.
- Verify movement by clipboard-pasting `x` and observing its logical/visual insertion point.
- For combining marks, move over a base with Arabic marks and press Backspace once. The base and all combining marks must disappear together.

## Structures, tabs, syntax, and rendering

- Use the actual saved notation when constructing fixtures:
  - Root: `$(√ text)`
  - Document-level Tab: `$(t)`, not a raw `\t`
  - Cases: `$({[first][second])`
- A single line containing `$(t)` must wrap normally.
- Two adjacent lines containing `$(t)` must align, remain unwrapped, and expose horizontal scrolling.
- For syntax checks, use a unique Markdown file containing a heading with annotated/ruby text and a separate `$(√ 49)+$(√ 81)` line. Heading/ruby text should inherit heading color; root strokes, numbers, and plus should remain ordinary unless the visible document syntax actually classifies them.
- For radical rendering, use nested/tall content such as `$(√ $(√ $(a/b)))` and inspect at high zoom. The hook and overbar must meet at one point with one stroke width.

## Verification discipline

- Never declare a GUI issue fixed from unit tests alone.
- Keep separate notes for model tests, WebView screenshots, native dialog behavior, and saved-file contents.
- If an automation action fails, inspect the current window/dialog before retrying. Do not continue sending keys blindly.
- Remove only temporary files created by the current test.
