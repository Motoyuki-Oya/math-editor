---
name: testing-mathnote
description: How to run and GUI-test the MathNote Tauri v2 + Leptos desktop editor on Linux, including workarounds for non-ASCII input and the unsaved-changes dialog.
---

# Testing MathNote (Tauri v2 + Leptos CSR)

## Running the app
- `cd <repo> && cargo tauri dev` (first cold build takes ~4-6 min; `trunk` + `cargo-tauri` must be on PATH).
- The window is titled `MathNote`. Bring it up and maximize with:
  `wmctrl -a MathNote && wmctrl -r MathNote -b add,maximized_vert,maximized_horz`
- **Beware: `wmctrl -a MathNote` can focus the wrong window.** The app's page title makes a Chrome tab on `localhost:1420` show up as `無題.txt — MathNote - Google Chrome for Testing`, and `wmctrl -a` matches it first. Get the real window id from `wmctrl -l` (the entry whose title is exactly `MathNote`) and use ids:
  `xdotool windowactivate <id>; wmctrl -i -r <id> -b add,maximized_vert,maximized_horz`
  Verify with `xdotool getactivewindow getwindowname` before trusting any screenshot.
- **Rebuilding after a new commit:** the editor lives in the *frontend* crate, which `trunk` compiles to wasm. A fast `Finished dev profile in 0.15s` line only covers `src-tauri`; confirm the frontend really rebuilt by looking for `Compiling mathnote-ui` + `applying new distribution` + `✅ success` in the log, with a timestamp after the commit. Also `kill` any `target/debug/mathnote` process left from an earlier build (`ps -eo pid,lstart,cmd | grep target/debug/mathnote`), otherwise you may be looking at a stale window serving old wasm.
- A harmless `dbind-WARNING ... org.a11y.Bus` line in the log is expected; it does not mean failure.
- Logs: redirect to a file (`cargo tauri dev > /tmp/tauri.log 2>&1 &`) since there is no devtools-friendly console.

## Known environment limitations / workarounds
- **Non-ASCII keyboard input does not work.** `xdotool type` silently drops Japanese (日本語), `√`, `α`, etc. — both in the contenteditable body and in the search inputs. Workarounds:
  - Put the non-ASCII text in a file (e.g. `/tmp/x.txt`) and load it with the 開く button.
  - To get non-ASCII into an input field: select the text in the document (a double-click selects a Japanese word cleanly), `Ctrl+C`, click the input, `Ctrl+V`. No `xclip`/`xsel` is installed, so shell-side clipboard tricks are unavailable.
  - Keyboard triggers that require typing a non-ASCII glyph (e.g. `√` + space) may be impossible to test directly; use the equivalent `\sqrt` + space or the palette button and report the glyph trigger as untested.
- **Do not use `window.confirm()` in this app.** It returns `false` in the Linux webkit2gtk WebView, which once made 新規 / 開く silently do nothing while unsaved. The unsaved-changes question now goes through the `confirm_discard` Tauri command (native dialog, buttons 破棄する / キャンセル); when testing 新規 / 開く while 未保存, expect that dialog and answer it.
- **Native GTK file dialogs are usable.** In the save/open dialog, `Ctrl+A` in the name field then typing a full absolute path (e.g. `/tmp/out.txt`) and pressing `Return` works. For opening arbitrary extensions, pick the「すべてのファイル」filter from the dropdown at the bottom-right first.

## Measuring the self-drawn caret and selection (VS Code-style editor core)
The editor draws its own caret (`.mn-cursor`, 2 CSS px wide) and selection (`.mn-sel`) into `.mn-overlay`; the real focus target is an off-screen `textarea.mn-input`.
- **The caret is easy to miss in screenshots.** On a maximized window the screenshot is downscaled to the tool's 1024x768 space, and a 2px line blends away. Zoom into a **narrow** region (roughly ≤80 tool px wide, e.g. `[8,140,80,162]`) to see it; wide regions like `[0,130,300,200]` can render the caret invisible even when it is there. Never conclude "no caret" from a wide screenshot — confirm with a tight zoom, and take 2-3 frames because it blinks (~1.1 s).
- **Measure character boundaries instead of guessing pixels.** The body font is proportional (~5-6 tool px per ASCII char), so press `Home` then `Shift+Right` N times and zoom on the highlight's right edge to find the boundary for column N; click there. After clicking, zoom again and read the caret's position between the glyphs before pressing any key — for multi-cursor tests you can see all carets at once.
- **Always confirm cursor positions by their effect, not only by looking.** Type one character and read the resulting string plus `N 文字 / M 行`; several different caret columns can produce the same character count (e.g. splitting `ab cd` at col 2 or col 3 both give 7 chars after typing one char per caret), so design the assertion so a wrong position yields a visibly different string.
- **Multi-cursor drift is best caught across two operations**, since a single `edit_each` pass can look correct either way. Good adversarial cases: (1) two carets, type `X` at both, then one `Backspace` — each caret must delete its own `X` (`AAA XBBBX` → `AAA BBB`); (2) carets before each of the two spaces in `ab cd ef`, one `Delete` — correct gives `abcdef`, drifting gives `abcd f`.
- `xdotool` double-click for word selection **registers intermittently** (sometimes handled as two single clicks). Retry, or select with `Shift+Arrow`/`Ctrl+D` when the test does not specifically target double-click.

## Clicking inside a formula lands at the wrong spot
Clicking a fraction's numerator/denominator inserts at the *formula's* start or end rather than in the clicked slot. Root cause (as of the editor-core branch): `math::render::position_at_point` scores **every** `[data-pos]` element by `dy*4 + dx` around each element's horizontal middle, and `data-pos` is set unconditionally on every node (`src/math/render.rs`), including the **outer fraction node at root level**. That ancestor's box contains the click (so `dy == 0`) and its middle nearly coincides with the numerator's, so it wins the tie — it is evaluated first in document order and children only win on a strictly smaller score.
- Diagnostic that needs no devtools: click the **left** half of the formula, then the **right** half. If the insertion flips from before the formula to after it, `position_at_point` is returning a root-level `Some(..)` (ancestor winning), not `None`. If it returned `None`, `enter_math(.., from_start=true)` would put the caret at the start regardless of where you click.

## Useful UI facts (for locating things)
- Row 1 toolbar: 新規 / 開く / 保存 / 名前を付けて / HTML出力 / 数式 / 数式(行) / 検索.
- Row 2 palette group 1 = structures (½ √ ⁿ√ x² xₙ ∑ ∏ ∫ lim (⋮) {⋮) which create formula boxes; group 2 = symbols/greek and group 3 = functions (sin…), which insert **plain text**.
- Search bar (Ctrl+F or 検索): 検索 input, 次を検索, 置換後 input, すべて置換, `Aa` (case sensitive), `.*` (regex), 閉じる.
- Status bar (bottom): file name, 未保存/保存済み, `N 文字 / M 行`, and a status message on the right — the character counter is the most reliable objective assertion for undo/redo steps. It is updated by `changed()`, so if a key handler is wired through a non-`edit()` path the counter can lag behind the text; when a counter looks stale, cross-check the body pixels before calling it a pass or a fail.
- To assert the dirty flag flips on an edit, save first (名前を付けて → a `/tmp` path) so the status reads `保存済み`, then press the key and check it becomes `未保存` immediately.
- Body triggers require an alphanumeric run before them: `abc 1/` works, but `abc /` does not (a `/` after a space is prose). Note `abc1/` makes `abc1` the numerator.
- Undo history coalesces changes within 700 ms (`COALESCE_MS` in `src/doc.rs`), so wait ~1 s between edits when you want distinct undo steps.

## Verifying output textually
Prefer asserting on saved files with the shell rather than only on pixels:
- 名前を付けて → `/tmp/x.md`, then `cat /tmp/x.md` — formulas must appear as `$\frac{1}{2}$` and plain symbols (`α`, `sin`) must NOT be wrapped in `$`.
- Round-trip: `diff` the reopened/re-saved file against the original (only a trailing-newline difference is expected).
- HTML出力 → `grep -o "<math[^>]*" /tmp/out.html` and check for `<mfrac>`.

## Devin Secrets Needed
None — the app is fully local with no auth or network dependency.
