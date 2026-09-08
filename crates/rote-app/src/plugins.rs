use std::path::PathBuf;

/// Where to look for `*.py` plugins, in load order. Both are optional and
/// missing directories are silently skipped by `PluginHost::load_dir`:
///
/// - `<config dir>/rote/plugins` — the real, per-user location
///   (`~/.config/rote/plugins` on Linux, `%APPDATA%\rote\plugins` on
///   Windows).
/// - `./plugins` — relative to the current directory, so `cargo run` from
///   the repo picks up `plugins/` during development without installing
///   anything.
pub fn plugin_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(config) = user_config_dir() {
        dirs.push(config.join("rote").join("plugins"));
    }
    dirs.push(PathBuf::from("plugins"));
    dirs
}

#[cfg(target_os = "windows")]
fn user_config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

#[cfg(not(target_os = "windows"))]
fn user_config_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
}
