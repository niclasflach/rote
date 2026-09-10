# Rote

A text editor written in Rust, with an embedded Python plugin API and a
GPU-rendered, minimal UI. This repo is an early scaffold: the architecture
is in place and it runs — a real editing core, real font rendering, real
LSP transport, real embedded Python — but most editor *behavior* (modal
editing, syntax highlighting, panes, a command palette) is still to build.

## Why this stack

| Concern | Choice | Why |
|---|---|---|
| Rendering | [`wgpu`](https://wgpu.rs) + [`winit`](https://github.com/rust-windowing/winit) + [`glyphon`](https://github.com/grovesNL/glyphon) (cosmic-text) | Full control over UI feel, proper text shaping/ligatures, and per-run font switching — the same category of stack Zed uses. Runs on Vulkan/DX12/Metal, so Linux and Windows share one renderer. |
| Editing core | [`ropey`](https://github.com/cessen/ropey) | O(log n) edits on large files; no rendering or UI knowledge, so it stays testable headlessly. |
| Plugins | Embedded Python via [`pyo3`](https://pyo3.rs) (`auto-initialize`) | Plugins get `import rote` and call straight into the running editor, in-process — no RPC, no serialization overhead, same shape as Neovim's built-in Lua API but for Python. |
| LSP | Hand-rolled JSON-RPC client over stdio + [`lsp-types`](https://github.com/gluon-lang/lsp-types) | Language servers are just subprocesses (`rust-analyzer`, `pyright`, `clangd`, ...) — the same model LazyVim/mason use. No language intelligence is reimplemented here. |

Trade-off worth knowing: building the UI on raw `wgpu` means every widget
(gutter, status line, panes, command palette) gets built by hand rather than
pulled from a widget toolkit. That's the cost of the "avskalat men modernt"
(minimal but modern) look and full typography control.

## Layout

```
rote/
├── crates/
│   ├── rote-core     — Buffer (rope), Cursor, Editor. No rendering/UI/Python knowledge.
│   ├── rote-render   — wgpu surface + glyphon text pipeline. Takes laid-out text, draws pixels.
│   ├── rote-ui       — retained-mode widgets (panels, gutter, status line, command palette).
│   │                    currently a placeholder; the MVP status line lives directly in rote-app.
│   ├── rote-lsp      — LSP client: spawns servers, speaks Content-Length-framed JSON-RPC.
│   ├── rote-pyapi    — embedded CPython + the `rote` module plugins import.
│   └── rote-app      — the `rote` binary: winit event loop, wires everything together.
└── plugins/          — example plugins, also picked up from `./plugins` when run via `cargo run`.
```

`rote-core` has zero dependencies on the other crates on purpose — it should
stay usable headlessly (a future test harness, or a batch/headless mode).

## Building

Requires:

- Rust (stable). This repo pins a version via `rust-toolchain.toml` /
  `.mise.toml` (the latter if you use [mise](https://mise.jdx.dev)).
- A Vulkan (Linux) or DirectX 12 (Windows) capable GPU/driver — `wgpu` also
  falls back to OpenGL/DX11 where available.
- A Python 3 installation with development headers, for `pyo3`:
  - Linux: your distro's `python3-dev`/`python3-devel` package (or just a
    normal Python install — most distro Pythons ship the headers already).
  - Windows: the official python.org installer (make sure "Add to PATH" and
    the dev libs are included); MSVC toolchain (`rustup target add
    x86_64-pc-windows-msvc` if cross-compiling from Linux, or just build
    natively on Windows).

```sh
cargo run -p rote-app            # scratch buffer
cargo run -p rote-app -- path/to/file.txt
cargo test --workspace           # rote-core's buffer/editor tests
```

Cross-compiling to Windows from Linux needs `cargo-xwin` or a MSVC
sysroot for the Python linkage to resolve; building natively on Windows (or
in CI on a Windows runner) is the path of least resistance for now.

## Current MVP controls

- Type to insert, Backspace/Delete to remove a char (or the selection, if
  any), Enter for newline.
- Arrow keys move the cursor; hold `Shift` to extend a selection.
  `Ctrl+Left`/`Ctrl+Right` jump by word, `Home`/`End` jump to line
  start/end, `Ctrl+Home`/`Ctrl+End` jump to the start/end of the buffer.
- Click to place the cursor, drag to select. `Ctrl+A` selects all.
- `Ctrl+S` — save (only if the buffer was opened from a file).
- `Ctrl+Shift+H` — bound entirely from Python by `plugins/hello.py` via
  `rote.bind_key`, demonstrating the plugin round-trip end to end.
- `Ctrl+P` — `plugins/file_picker.py`'s file-open overlay: type to filter,
  `Up`/`Down` to move, `Enter` to open the selected file, `Esc` to cancel.
- `Esc` — quit (or close a picker overlay, if one is open).

The body text doesn't soft-wrap: long lines run off the right edge instead
of wrapping, and there's no horizontal scroll yet either. This is
deliberate for now, not just unfinished — a gutter plugin's rows need to
line up 1:1 with the body's, and wrapping breaks that without a
wrapped-row-aware gutter API to go with it (see Roadmap).

## Plugin API (current surface)

```python
import rote

rote.text()                       # -> str, the active buffer's full text
rote.insert(text)                 # insert at the cursor, cursor advances past it
rote.backspace()                  # delete one char before the cursor
rote.save()                       # write the active buffer to its path
rote.open(path)                   # load a file into a new buffer, making it active
rote.register_command(name, fn)   # bind a Python callable to a name
rote.register_gutter(fn)          # fn(line, wrapped, current) -> str, called per body line
rote.bind_key(chord, fn)          # e.g. "ctrl+shift+p" — run fn() when that chord is pressed
rote.open_picker(items, on_select)  # show an in-editor overlay list; on_select(item) on Enter
```

Drop a `.py` file in `~/.config/rote/plugins` (`%APPDATA%\rote\plugins` on
Windows) or `./plugins` next to where you run `cargo run`, and it's loaded
at startup with `import rote` already available — see `plugins/hello.py`.
A plugin that raises on load is logged and skipped; it won't take the editor
down with it.

`register_gutter` draws a column to the left of the body text — line
numbers, diagnostics/git signs, breakpoints, whatever a plugin wants to show
per line. Rote calls `fn` once per logical line on every relayout (not once
per frame), passing the 1-based line number, whether the row is a soft-wrap
continuation (always `False` today — the body text doesn't wrap yet, see
Roadmap), and whether it's the buffer's current line; it returns the string
to render for that row. The gutter column is only reserved (and drawn) once
a plugin has actually registered one — see `plugins/line_numbers.py`.

`bind_key` takes a chord like `"ctrl+shift+p"` or `"F2"` (modifier order and
case don't matter — it's normalized before matching) and a zero-arg
callable to run when that chord is pressed; `Ctrl+Shift+H` in
`plugins/hello.py` is bound this way. A handful of chords are still
hardcoded ahead of plugin keymaps and can't be overridden — arrow keys,
Home/End, `Ctrl+S`, `Ctrl+A`, and `Esc` (which quits, or closes a picker,
before a key event even reaches plugin dispatch) — everything else falls
through to whatever a plugin bound.

`open_picker(items, on_select)` shows a filterable, keyboard-navigable list
over the body area — typing filters it (plain substring match), `Up`/`Down`
moves the selection, `Enter` calls `on_select(chosen_item)` and closes it,
`Esc` closes it without calling anything. `rote-pyapi` only stores the item
list and callback; `rote-app` owns the actual overlay (query text, the
selected row, rendering), so opening one doesn't need any more plumbing
than that single call — see `plugins/file_picker.py`, which lists the
working directory with `os.walk` (no Rote API needed for that part — a
plugin is a full CPython) and passes `rote.open` itself as `on_select`.
While a picker is open it captures all keyboard input; nothing reaches the
buffer underneath until it closes.

This surface will grow (cursor motion, selections, buffer events) as the
editing core does — see `PluginHost::install_module` in
`crates/rote-pyapi/src/lib.rs`.

## Roadmap

Roughly in the order that unblocks the most, inspired by what makes LazyVim
pleasant day to day:

1. ~~**Cursor motion & selection**~~ — done: arrow keys, mouse clicks/drag,
   word/line motions, a rendered caret and selection highlight (a small
   custom `wgpu` quad pipeline in `rote-render`, since glyphon only
   rasterizes glyphs).
2. **Syntax highlighting** — `tree-sitter`, feeding per-span `Attrs` (color,
   weight) into the same `glyphon::Buffer` already used for rendering.
3. **`rote-lsp` wired into `rote-app`** — the client exists but nothing
   calls it yet: diagnostics as underlines, completion popup, hover, go to
   definition.
4. **`rote-ui`** — a *built-in* gutter (today's line numbers only exist via
   `plugins/line_numbers.py` and `rote.register_gutter`), status line (move
   it out of `rote-app`), splits. Soft-wrap plus a wrap-aware
   `register_gutter` (so continuation rows are distinguishable and a
   gutter can span them) belongs here too. A fuzzy-find command palette
   mostly already exists as `rote.open_picker` (see Plugin API) — what's
   missing is fuzzy (vs. substring) matching and floating the overlay on
   top of the body instead of replacing it.
5. **Modal editing / keymap system** — a LazyVim-style layered keymap
   (default → language → user overrides), not hardcoded `match` arms in
   `main.rs` like today.
6. **Config file** — theme, font, keymaps; likely TOML, loaded before
   plugins so plugins can read it via the `rote` module.
7. **Windows packaging** — installer / portable zip once the above is
   stable enough to be worth shipping.
