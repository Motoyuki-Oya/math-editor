---
name: testing-mathnote
description: How to run and GUI-test the Planetext (formerly MathNote) Tauri v2 + Leptos desktop editor on Linux, including the WebKit inspector route, synthetic IME composition, the row/DOM contract, and workarounds for non-ASCII input.
---

# Testing Planetext (Tauri v2 + Leptos CSR)

The app was renamed **MathNote → Planetext**: binary `target/debug/planetext`, frontend crate `planetext-ui`,
window title exactly `Planetext`. Anything still saying `mathnote` (e.g. a `cargo clippy -p mathnote` lint command)
will fail with "package not found".

## Running the app
- `cd <repo> && GDK_SCALE=2 cargo tauri dev > /tmp/tauri.log 2>&1 &`.
  **Use `GDK_SCALE=2`**: at the default 15px font the caret, the dashed empty-slot box and fraction rules are
  impossible to judge in a downscaled screenshot. It changes no code and no CSS.
- Pick the window by **id**, never by name matching: a Chrome tab on `localhost:1420` shows up as
  `無題.txt — Planetext - Google Chrome for Testing` and `wmctrl -a` matches it first.
  `wmctrl -lG | grep -i "Planetext$"` → `xdotool windowactivate <id>; wmctrl -i -r <id> -b add,maximized_vert,maximized_horz`,
  then verify with `xdotool getactivewindow getwindowname`.
- **Confirm the frontend really rebuilt.** `Finished dev profile in 0.15s` only covers `src-tauri`. Look for
  `Compiling planetext-ui` + `applying new distribution` + `✅ success` with a timestamp after the commit, and
  kill stale `target/debug/planetext` processes (`ps -eo pid,lstart,cmd | grep target/debug/planetext`).
- `trunk serve` watches the tree: **if anyone edits the checkout while you test, the window is replaced by a
  "Build failure" overlay** and your evidence stops being about the revision under test. If you share a checkout
  with another agent, ask for a separate worktree/clone (own `target`, own trunk port) before starting.
- After a rebuild, `Ctrl+R` in the window reloads the webview; check the wasm file name in the console
  (`planetext-ui-<hash>_bg.wasm`) to be sure you are on the new build.

## WebKit inspector is available (this is the console/panic route)
Right-click inside the app → **Inspect Element** → full WebKit inspector (Elements / Console / Sources / Computed).
`console_error_panic_hook::set_once()` is installed, so **Rust/WASM panics appear in this Console**.
- Docking the inspector shrinks the document area; **close it (the ✕ at the inspector's top-left) before taking
  the screenshots that are meant to show the rendered formula.**
- Clicking into the console **blurs the app**, and the app only draws carets when focused
  (`focused && caret.composing.is_none()`), so `document.querySelector('.mn-cursor')` is `null` while you type in
  the console. Either judge the caret from pixels, or arm a delayed measurement
  (`ta().focus(); setTimeout(()=>{window.__m=...},3000)`) and read `window.__m` afterwards.
- Console output is truncated in a screenshot when long: `console.log` one line per selector instead of returning
  one big JSON string, and zoom on the console area.

## The row/DOM contract (post "one row component" refactor)
Every row at every depth is drawn by `src/view/row.rs`:
- `span.mn-row[data-path]` — **the document's own line is the row whose `data-path` is empty**; inside a structure
  the path is `index.slot` pairs joined by `,` (e.g. `0.0` = island at col 0, `0.0,0.1` = its fraction's
  denominator, `0.0,0.1,0.1` = a fraction nested in that denominator).
- Characters: `.mn-run` in prose, `.mn-atom mn-num|mn-ident|mn-word|mn-bin|mn-punct` inside formulas (so a `+g`
  is **two** elements — never match atoms by multi-character text). Islands: `.mn-field`. Empty slot: `.mn-placeholder`.
- Carets and selections are **overlay rectangles** `.mn-cursor` / `.mn-sel` in `.mn-overlay`, not inline spans
  (`.mn-caret`, `.mn-placeholder-here`, `.mn-field-active` no longer exist).
- IME preedit: `.mn-preedit` (underline + accent background) inserted **inside the row the caret is in**.
  The assertion to write is
  `document.querySelector('.mn-preedit').closest('.mn-row').dataset.path === '<expected path>'`,
  plus a screenshot proving it is visible in that slot.

## Driving IME without a real IME
A real IBus/IME cannot be driven on this box (XTEST input never reaches IBus) — report real IME as **untested**
and use synthetic composition events from the console:
```js
const ta = () => document.querySelector('textarea.mn-input');
function comp(text){ const t=ta(); t.focus();
  t.dispatchEvent(new CompositionEvent('compositionstart',{bubbles:true,data:''}));
  t.value=text;
  t.dispatchEvent(new CompositionEvent('compositionupdate',{bubbles:true,data:text}));
  return [...document.querySelectorAll('.mn-preedit')].map(e=>e.textContent+'#'+e.closest('.mn-row').dataset.path);}
function endComp(text){ const t=ta();
  t.dispatchEvent(new CompositionEvent('compositionend',{bubbles:true,data:text})); t.value='';}
```
Pass Japanese as escapes (`comp('\u306b\u307b\u3093')`) — non-ASCII cannot be typed.
**Always finish with the two adversarial checks**: after `endComp(text)` the characters must land in the same row,
and a following ordinary keypress must still register — `state::on_keydown` returns early while `composing` is
true, so a composition left open makes the app swallow every key, which users report as 「固まる」.
Also send `endComp('')` (cancelled conversion) and check keys still work.

## Structure editing semantics that trip tests up
- Body triggers need an alphanumeric run: `abc 1/` makes a fraction, `abc /` stays prose, and `abc1/` uses `abc1`
  as the numerator. `$` starts an empty island; inside an island `(`/`[` open a group, `/` stacks, `\sqrt ` and
  `√`+space expand a root.
- **`)` only leaves the enclosing group, never the fraction/root around it.** After `1/(2/3)` a `)` puts you back
  in the *denominator* row, so `1/(2/3)+4` saves as `$(1/($(2/3))+4)` with `+4` **inside** the denominator. If a
  test expects trailing text on the outer baseline, you must leave the structure with arrow keys (`Right`) instead
  of typing `)`, otherwise you are measuring the wrong row and will misattribute a structural placement to CSS.
- `Tab` inside an island is not a column separator (columns exist only on document lines); it moves the caret out
  of / between slots.
- A drag that starts inside an island resolves through `pos_at_point`, which takes an island **as a whole**: you
  get a document selection of the whole island (Delete removes the entire formula), not an in-row selection.
  Use `Shift+Left/Right` for a selection inside a structure.
- Undo coalesces (~700 ms); a fraction created and typed into in one burst can undo to an empty island.

## Judging pixels
- Zoom into **narrow** regions (≤ ~120 tool px wide); a 2px caret disappears in a wide zoom. Take 2-3 frames
  because it blinks (~1.1 s) — one frame with and one without the caret is the strongest evidence for
  "the caret is inside the dashed empty-slot box".
- `N 文字` counts an island as 1 character, so it cannot distinguish `1` from `$(1/)`. **Save (名前を付けて →
  `/tmp/x.txt`) and `cat` the file** to establish what the document really contains.
- For baselines, measure instead of eyeballing: compare `getBoundingClientRect()` of `.mn-frac-rule` with the
  centre y of the runs around it (≤1.5px = aligned). Check `getComputedStyle` of `.mn-frac` (`inline-grid`,
  `vertical-align: middle`), `.mn-row` (`inline`), `.mn-group` / `.mn-sqrt` (`inline-block; position: relative`).
  If `grid-template-rows` reports equal `1fr` rows, the rule is centred and text outside the fraction should land
  on it.

## Native dialogs / non-ASCII
- GTK file dialogs work: `Ctrl+A` in the name field, type an absolute path, `Return` (choose「すべてのファイル」
  for odd extensions).
- Non-ASCII keyboard input is impossible (`xdotool type` drops it) and no `xclip`/`xsel` is installed. Put the text
  in a file, 開く it, select with `Home`/`Shift+End`, `Ctrl+C`, then paste where you need it — this is also how you
  test the `√` glyph trigger.
- Do not rely on `window.confirm()`; the unsaved-changes question is a native dialog (破棄する / キャンセル).
- Harmless log noise: `dbind-WARNING ... org.a11y.Bus`, `Gtk-CRITICAL ... WIDGET_REALIZED_FOR_EVENT` (file dialogs).
  Grep the log for real failures: `grep -iE "panic|already borrowed|RuntimeError|index out of bounds" /tmp/tauri.log`.

## Devin Secrets Needed
None — the app is fully local with no auth or network dependency.
