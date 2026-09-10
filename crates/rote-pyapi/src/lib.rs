//! Embedded Python plugin API. Rote embeds a CPython interpreter (via
//! PyO3's `auto-initialize`) rather than shelling out to plugin
//! subprocesses — plugins under `~/.config/rote/plugins/*.py` get an
//! `import rote` module backed directly by the running [`rote_core::Editor`],
//! the same shape as Neovim's built-in Lua API but for Python.
//!
//! This is intentionally a thin, growable surface: `text`/`insert`/`save`/
//! `open` plus `register_command` for binding a Python callable to a name
//! the app can later invoke from a keymap or the command palette,
//! `register_gutter` for a callable that supplies per-line gutter text
//! (line numbers, diagnostics/git signs, ...), `bind_key` for binding a
//! callable straight to a key chord (`rote-app` still owns a handful of
//! hardcoded bindings — arrows, Ctrl+S, Esc — that a plugin can't
//! override), and `open_picker` for showing an in-editor overlay list and
//! getting back whichever item the user chose (see `plugins/file_picker.py`
//! for a `Ctrl+P` file-open picker built on it). Add functions to
//! [`PluginHost::install_module`] as the editor grows more to expose.

use std::collections::HashMap;
use std::ffi::CString;
use std::path::Path;
use std::sync::{Arc, Mutex};

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyModule, PyTuple};

use rote_core::Editor;

pub type SharedEditor = Arc<Mutex<Editor>>;
type CommandRegistry = Arc<Mutex<HashMap<String, Py<PyAny>>>>;
type GutterProvider = Arc<Mutex<Option<Py<PyAny>>>>;
/// Keyed by the chord's canonical form (see `rote_app`'s key-chord
/// normalization — the two sides of this API have to agree on one string
/// shape for a lookup to ever hit).
type KeymapRegistry = Arc<Mutex<HashMap<String, Py<PyAny>>>>;

/// A pending `rote.open_picker` call: the items to show and the callback
/// to run with whichever one the user picks. `rote-app` owns the actual
/// overlay (query text, filtering, the selected row, rendering) — this is
/// just the handoff between "a plugin asked for a picker" and "the user
/// answered it".
struct PickerRequest {
    items: Vec<String>,
    on_select: Py<PyAny>,
}
type PickerSlot = Arc<Mutex<Option<PickerRequest>>>;

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("python error: {0}")]
    Python(#[from] PyErr),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Owns the embedded interpreter's view of the editor: the `rote` module
/// plugins import, and the registry `rote.register_command` populates.
/// `rote-app` creates one `PluginHost` at startup, sharing the same
/// `Arc<Mutex<Editor>>` it drives from the winit event loop, and calls
/// [`PluginHost::load_dir`] once for the user's plugin directory.
pub struct PluginHost {
    editor: SharedEditor,
    commands: CommandRegistry,
    gutter: GutterProvider,
    keymap: KeymapRegistry,
    picker: PickerSlot,
}

impl PluginHost {
    pub fn new(editor: SharedEditor) -> Self {
        Self {
            editor,
            commands: Arc::new(Mutex::new(HashMap::new())),
            gutter: Arc::new(Mutex::new(None)),
            keymap: Arc::new(Mutex::new(HashMap::new())),
            picker: Arc::new(Mutex::new(None)),
        }
    }

    /// Build the `rote` module and register it in `sys.modules` so
    /// `import rote` resolves from any plugin script loaded afterwards.
    fn install_module(&self, py: Python<'_>) -> PyResult<()> {
        let module = PyModule::new(py, "rote")?;

        let editor = self.editor.clone();
        let text_fn = PyCFunction::new_closure(py, Some(c"text"), None, move |_args, _kwargs| {
            let editor = editor.lock().expect("editor mutex poisoned");
            PyResult::Ok(editor.active_buffer().map(|b| b.text()).unwrap_or_default())
        })?;
        module.add("text", text_fn)?;

        let editor = self.editor.clone();
        let insert_fn = PyCFunction::new_closure(py, Some(c"insert"), None, move |args: &Bound<'_, PyTuple>, _kwargs| {
            let (text,): (String,) = args.extract()?;
            let mut editor = editor.lock().expect("editor mutex poisoned");
            editor
                .insert_at_cursor(&text)
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            PyResult::Ok(())
        })?;
        module.add("insert", insert_fn)?;

        let editor = self.editor.clone();
        let backspace_fn = PyCFunction::new_closure(py, Some(c"backspace"), None, move |_args, _kwargs| {
            let mut editor = editor.lock().expect("editor mutex poisoned");
            editor
                .backspace_at_cursor()
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            PyResult::Ok(())
        })?;
        module.add("backspace", backspace_fn)?;

        let editor = self.editor.clone();
        let save_fn = PyCFunction::new_closure(py, Some(c"save"), None, move |_args, _kwargs| {
            let mut editor = editor.lock().expect("editor mutex poisoned");
            if let Some(buf) = editor.active_buffer_mut() {
                buf.save().map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            }
            PyResult::Ok(())
        })?;
        module.add("save", save_fn)?;

        let editor = self.editor.clone();
        let open_fn = PyCFunction::new_closure(py, Some(c"open"), None, move |args: &Bound<'_, PyTuple>, _kwargs| {
            let (path,): (String,) = args.extract()?;
            let mut editor = editor.lock().expect("editor mutex poisoned");
            editor
                .open_file(&path)
                .map_err(|e| PyRuntimeError::new_err(format!("{path}: {e}")))?;
            PyResult::Ok(())
        })?;
        module.add("open", open_fn)?;

        let picker = self.picker.clone();
        let open_picker_fn = PyCFunction::new_closure(
            py,
            Some(c"open_picker"),
            None,
            move |args: &Bound<'_, PyTuple>, _kwargs| {
                let (items, on_select): (Vec<String>, Py<PyAny>) = args.extract()?;
                *picker.lock().expect("picker poisoned") = Some(PickerRequest { items, on_select });
                PyResult::Ok(())
            },
        )?;
        module.add("open_picker", open_picker_fn)?;

        let commands = self.commands.clone();
        let register_fn = PyCFunction::new_closure(
            py,
            Some(c"register_command"),
            None,
            move |args: &Bound<'_, PyTuple>, _kwargs| {
                let (name, callback): (String, Py<PyAny>) = args.extract()?;
                commands.lock().expect("command registry poisoned").insert(name, callback);
                PyResult::Ok(())
            },
        )?;
        module.add("register_command", register_fn)?;

        let gutter = self.gutter.clone();
        let register_gutter_fn = PyCFunction::new_closure(
            py,
            Some(c"register_gutter"),
            None,
            move |args: &Bound<'_, PyTuple>, _kwargs| {
                let (callback,): (Py<PyAny>,) = args.extract()?;
                *gutter.lock().expect("gutter provider poisoned") = Some(callback);
                PyResult::Ok(())
            },
        )?;
        module.add("register_gutter", register_gutter_fn)?;

        let keymap = self.keymap.clone();
        let bind_key_fn = PyCFunction::new_closure(
            py,
            Some(c"bind_key"),
            None,
            move |args: &Bound<'_, PyTuple>, _kwargs| {
                let (chord, callback): (String, Py<PyAny>) = args.extract()?;
                let canonical = canonicalize_chord(&chord)
                    .ok_or_else(|| PyRuntimeError::new_err(format!("rote.bind_key: invalid key chord {chord:?}")))?;
                keymap.lock().expect("keymap registry poisoned").insert(canonical, callback);
                PyResult::Ok(())
            },
        )?;
        module.add("bind_key", bind_key_fn)?;

        py.import("sys")?.getattr("modules")?.set_item("rote", module)?;
        Ok(())
    }

    /// Load and execute every `*.py` file directly under `dir` (not
    /// recursive), each as its own module with `import rote` available. A
    /// plugin that fails to load is logged and skipped — one broken plugin
    /// should never prevent the editor from starting.
    pub fn load_dir(&self, dir: &Path) -> Result<usize, PluginError> {
        if !dir.is_dir() {
            return Ok(0);
        }
        let mut loaded = 0;
        Python::attach(|py| -> Result<(), PluginError> {
            self.install_module(py)?;
            for entry in std::fs::read_dir(dir)? {
                let path = entry?.path();
                if path.extension().and_then(|e| e.to_str()) != Some("py") {
                    continue;
                }
                let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
                let src = std::fs::read_to_string(&path)?;

                let code = CString::new(src).unwrap_or_default();
                let file = CString::new(file_name.clone()).unwrap_or_default();
                let module_name = CString::new(file_name.trim_end_matches(".py")).unwrap_or_default();

                match PyModule::from_code(py, code.as_c_str(), file.as_c_str(), module_name.as_c_str()) {
                    Ok(_) => {
                        tracing::info!("loaded plugin {file_name}");
                        loaded += 1;
                    }
                    Err(err) => {
                        err.print(py);
                        tracing::warn!("plugin {file_name} failed to load");
                    }
                }
            }
            Ok(())
        })?;
        Ok(loaded)
    }

    /// Invoke a command a plugin registered via `rote.register_command`.
    /// A silent no-op if no plugin registered `name`.
    pub fn run_command(&self, name: &str) -> Result<(), PluginError> {
        let commands = self.commands.clone();
        let name = name.to_string();
        Python::attach(|py| -> Result<(), PluginError> {
            let callback = commands
                .lock()
                .expect("command registry poisoned")
                .get(&name)
                .map(|p| p.clone_ref(py));
            if let Some(callback) = callback {
                callback.call0(py)?;
            }
            Ok(())
        })
    }

    /// Whether a plugin has called `rote.register_gutter`. `rote-app` uses
    /// this to decide whether to reserve gutter width at all — most buffers
    /// have no gutter plugin loaded, and shaping/measuring an all-blank
    /// gutter buffer every frame would be wasted work.
    pub fn has_gutter(&self) -> bool {
        self.gutter.lock().expect("gutter provider poisoned").is_some()
    }

    /// Ask the registered gutter provider what to render for one line of
    /// the body text. `line` is the 1-based logical line number; `wrapped`
    /// is true for a soft-wrap continuation row (not the line's first —
    /// always `false` today, since `rote-app` doesn't wrap the body text
    /// yet); `current` is true if `line` is the buffer's active cursor
    /// line. Returns `None` if no plugin registered a provider, or if the
    /// callback raises (logged, not fatal — one bad gutter plugin
    /// shouldn't blank the whole gutter).
    pub fn gutter_text(&self, line: usize, wrapped: bool, current: bool) -> Option<String> {
        let gutter = self.gutter.clone();
        Python::attach(|py| {
            let callback = gutter.lock().expect("gutter provider poisoned").as_ref()?.clone_ref(py);
            match callback.call1(py, (line, wrapped, current)) {
                Ok(result) => match result.extract::<String>(py) {
                    Ok(text) => Some(text),
                    Err(err) => {
                        err.print(py);
                        None
                    }
                },
                Err(err) => {
                    err.print(py);
                    None
                }
            }
        })
    }

    /// The item list from a plugin's pending `rote.open_picker` call,
    /// without clearing it — the request (including its `on_select`
    /// callback) stays stored until [`Self::confirm_picker`] or
    /// [`Self::cancel_picker`] resolves it. `rote-app` calls this right
    /// after dispatching a keymap/command, to notice a picker was just
    /// requested and start driving its own overlay (query, filtering, the
    /// selected row — none of which `PluginHost` knows anything about).
    /// `None` if no picker is pending.
    pub fn pending_picker_items(&self) -> Option<Vec<String>> {
        self.picker.lock().expect("picker poisoned").as_ref().map(|r| r.items.clone())
    }

    /// Calls the pending picker's `on_select` with `chosen`, then clears
    /// the request. A no-op if there is no pending picker.
    pub fn confirm_picker(&self, chosen: &str) -> Result<(), PluginError> {
        let request = self.picker.lock().expect("picker poisoned").take();
        if let Some(request) = request {
            Python::attach(|py| -> Result<(), PluginError> {
                request.on_select.call1(py, (chosen,))?;
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Clears a pending picker request without calling its callback —
    /// the user cancelled (Esc) rather than picking something.
    pub fn cancel_picker(&self) {
        self.picker.lock().expect("picker poisoned").take();
    }

    /// Invoke whatever a plugin bound to `chord` via `rote.bind_key`,
    /// already in canonical form (see [`canonicalize_chord`]/[`build_chord`]
    /// — `rote-app` builds it straight from the winit key event, in the
    /// same shape `canonicalize_chord` normalizes a plugin's string into).
    /// A silent no-op if nothing is bound to it, same as [`Self::run_command`].
    pub fn run_keymap(&self, chord: &str) -> Result<(), PluginError> {
        let keymap = self.keymap.clone();
        let chord = chord.to_string();
        Python::attach(|py| -> Result<(), PluginError> {
            let callback = keymap.lock().expect("keymap registry poisoned").get(&chord).map(|p| p.clone_ref(py));
            if let Some(callback) = callback {
                callback.call0(py)?;
            }
            Ok(())
        })
    }
}

/// Normalizes a key-chord string a plugin passed to `rote.bind_key` (e.g.
/// `"Ctrl+Shift+P"`, `"ctrl+p"`, `"F2"`) into the canonical form
/// [`build_chord`] produces, so a lookup against a chord built from a live
/// key event doesn't care how the plugin author capitalized or ordered it.
/// Returns `None` if `raw` names modifiers only, with no actual key.
pub fn canonicalize_chord(raw: &str) -> Option<String> {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut sup = false;
    let mut key = None;
    for part in raw.split('+') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" | "option" => alt = true,
            "shift" => shift = true,
            "super" | "cmd" | "command" | "meta" | "win" => sup = true,
            other => key = Some(other.to_string()),
        }
    }
    Some(build_chord(ctrl, alt, shift, sup, &key?))
}

/// Builds a canonical key-chord string from already-known modifier state
/// and a lowercase key name — modifiers in a fixed order, so this and
/// [`canonicalize_chord`] always agree on one shape for the same chord.
pub fn build_chord(ctrl: bool, alt: bool, shift: bool, sup: bool, key: &str) -> String {
    let mut chord = String::new();
    if ctrl {
        chord.push_str("ctrl+");
    }
    if alt {
        chord.push_str("alt+");
    }
    if shift {
        chord.push_str("shift+");
    }
    if sup {
        chord.push_str("super+");
    }
    chord.push_str(&key.to_ascii_lowercase());
    chord
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_is_order_and_case_insensitive() {
        assert_eq!(canonicalize_chord("Ctrl+Shift+P"), Some("ctrl+shift+p".to_string()));
        assert_eq!(canonicalize_chord("shift+ctrl+p"), Some("ctrl+shift+p".to_string()));
        assert_eq!(canonicalize_chord("P"), Some("p".to_string()));
        assert_eq!(canonicalize_chord("F2"), Some("f2".to_string()));
        assert_eq!(canonicalize_chord("ctrl+shift"), None);
    }

    #[test]
    fn build_chord_matches_canonicalize() {
        assert_eq!(build_chord(true, false, true, false, "p"), "ctrl+shift+p");
        assert_eq!(canonicalize_chord("Shift+Ctrl+P"), Some(build_chord(true, false, true, false, "p")));
    }
}
