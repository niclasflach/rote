//! Embedded Python plugin API. Rote embeds a CPython interpreter (via
//! PyO3's `auto-initialize`) rather than shelling out to plugin
//! subprocesses — plugins under `~/.config/rote/plugins/*.py` get an
//! `import rote` module backed directly by the running [`rote_core::Editor`],
//! the same shape as Neovim's built-in Lua API but for Python.
//!
//! This is intentionally a thin, growable surface: `text`/`insert`/`save`
//! plus `register_command` for binding a Python callable to a name the app
//! can later invoke from a keymap or the command palette. Add functions to
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
}

impl PluginHost {
    pub fn new(editor: SharedEditor) -> Self {
        Self {
            editor,
            commands: Arc::new(Mutex::new(HashMap::new())),
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
}
