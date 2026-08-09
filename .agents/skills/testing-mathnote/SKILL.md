---
name: testing-mathnote
description: How to run and GUI-test the MathNote Tauri v2 + Leptos desktop editor on Linux, including workarounds for non-ASCII input and the unsaved-changes dialog.
---

# Testing MathNote (Tauri v2 + Leptos CSR)

## Running the app
- `cd <repo> && cargo tauri dev` (first cold build takes ~4-6 min; `trunk` + `cargo-tauri` must be on PATH).
- The window is titled `MathNote`. Bring it up and maximize with:
  `wmctrl -a MathNote && wmctrl -r MathNote -b add,maximized_vert,maximized_horz`
- A harmless `dbind-WARNING ... org.a11y.Bus` line in the log is expected; it does not mean failure.
- Logs: redirect to a file (`cargo tauri dev > /tmp/tauri.log 2>&1 &`) since there is no devtools-friendly console.

## Known environment limitations / workarounds
- **Non-ASCII keyboard input does not work.** `xdotool type` silently drops Japanese (日本語), `√`, `α`, etc. — both in the contenteditable body and in the search inputs. Workarounds:
  - Put the non-ASCII text in a file (e.g. `/tmp/x.txt`) and load it with the 開く button.
  - To get non-ASCII into an input field: select the text in the document (a double-click selects a Japanese word cleanly), `Ctrl+C`, click the input, `Ctrl+V`. No `xclip`/`xsel` is installed, so shell-side clipboard tricks are unavailable.
  - Keyboard triggers that require typing a non-ASCII glyph (e.g. `√` + space) may be impossible to test directly; use the equivalent `\sqrt` + space or the palette button and report the glyph trigger as untested.
- **Do not use `window.confirm()` in this app.** It returns `false` in the Linux webkit2gtk WebView, which once made 新規 / 開く silently do nothing while unsaved. The unsaved-changes question now goes through the `confirm_discard` Tauri command (native dialog, buttons 破棄する / キャンセル); when testing 新規 / 開く while 未保存, expect that dialog and answer it.
- **Native GTK file dialogs are usable.** In the save/open dialog, `Ctrl+A` in the name field then typing a full absolute path (e.g. `/tmp/out.txt`) and pressing `Return` works. For opening arbitrary extensions, pick the「すべてのファイル」filter from the dropdown at the bottom-right first.

## Useful UI facts (for locating things)
- Row 1 toolbar: 新規 / 開く / 保存 / 名前を付けて / HTML出力 / 数式 / 数式(行) / 検索.
- Row 2 palette group 1 = structures (½ √ ⁿ√ x² xₙ ∑ ∏ ∫ lim (⋮) {⋮) which create formula boxes; group 2 = symbols/greek and group 3 = functions (sin…), which insert **plain text**.
- Search bar (Ctrl+F or 検索): 検索 input, 次を検索, 置換後 input, すべて置換, `Aa` (case sensitive), `.*` (regex), 閉じる.
- Status bar (bottom): file name, 未保存/保存済み, `N 文字 / M 行`, and a status message on the right — the character counter is the most reliable objective assertion for undo/redo steps.
- Body triggers require an alphanumeric run before them: `abc 1/` works, but `abc /` does not (a `/` after a space is prose). Note `abc1/` makes `abc1` the numerator.
- Undo history coalesces changes within 700 ms (`COALESCE_MS` in `src/doc.rs`), so wait ~1 s between edits when you want distinct undo steps.

## Verifying output textually
Prefer asserting on saved files with the shell rather than only on pixels:
- 名前を付けて → `/tmp/x.md`, then `cat /tmp/x.md` — formulas must appear as `$\frac{1}{2}$` and plain symbols (`α`, `sin`) must NOT be wrapped in `$`.
- Round-trip: `diff` the reopened/re-saved file against the original (only a trailing-newline difference is expected).
- HTML出力 → `grep -o "<math[^>]*" /tmp/out.html` and check for `<mfrac>`.

## Devin Secrets Needed
None — the app is fully local with no auth or network dependency.
