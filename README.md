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

- Type to insert, Backspace to delete, Enter for newline.
- `Ctrl+S` — save (only if the buffer was opened from a file).
- `Ctrl+Shift+H` — run the `hello` command registered by `plugins/hello.py`,
  demonstrating the plugin round-trip end to end.
- `Esc` — quit.

There's no cursor movement (arrow keys, clicking) or selection yet — see
Roadmap.

## Plugin API (current surface)

```python
import rote

rote.text()                       # -> str, the active buffer's full text
rote.insert(text)                 # insert at the cursor, cursor advances past it
rote.backspace()                  # delete one char before the cursor
rote.save()                       # write the active buffer to its path
rote.register_command(name, fn)   # bind a Python callable to a name
```

Drop a `.py` file in `~/.config/rote/plugins` (`%APPDATA%\rote\plugins` on
Windows) or `./plugins` next to where you run `cargo run`, and it's loaded
at startup with `import rote` already available — see `plugins/hello.py`.
A plugin that raises on load is logged and skipped; it won't take the editor
down with it.

This surface will grow (cursor motion, selections, buffer events, keymap
registration) as the editing core does — see `PluginHost::install_module` in
`crates/rote-pyapi/src/lib.rs`.

## Roadmap

Roughly in the order that unblocks the most, inspired by what makes LazyVim
pleasant day to day:

1. **Cursor motion & selection** — arrow keys, mouse clicks/drag, word/line
   motions. `rote-core::Cursor` already models a head + optional anchor.
2. **Syntax highlighting** — `tree-sitter`, feeding per-span `Attrs` (color,
   weight) into the same `glyphon::Buffer` already used for rendering.
3. **`rote-lsp` wired into `rote-app`** — the client exists but nothing
   calls it yet: diagnostics as underlines, completion popup, hover, go to
   definition.
4. **`rote-ui`** — gutter (line numbers, diagnostics/git signs), status
   line (move it out of `rote-app`), a fuzzy-find command palette, splits.
5. **Modal editing / keymap system** — a LazyVim-style layered keymap
   (default → language → user overrides), not hardcoded `match` arms in
   `main.rs` like today.
6. **Config file** — theme, font, keymaps; likely TOML, loaded before
   plugins so plugins can read it via the `rote` module.
7. **Windows packaging** — installer / portable zip once the above is
   stable enough to be worth shipping.
