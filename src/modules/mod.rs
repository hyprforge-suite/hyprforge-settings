pub mod appearance;
pub mod bluetooth;
pub mod catalog_screen;
pub mod default_apps;
pub mod desktop;
pub mod displays;
pub mod input;
pub mod keycapture;
pub mod layout_canvas;
pub mod network;
pub mod power;
pub mod session;
pub mod setup;
pub mod system;
pub mod setting_rows;
pub mod setup_notice;
pub mod shortcuts;
pub mod tray;
pub mod window_rules;

/// Evaluates the user's own `hyprland.lua` for importable entries.
///
/// Blocking work (a synchronous Lua VM run over their whole config), so it
/// goes on the blocking pool rather than stalling the UI thread. Shared
/// because all three modules import from the same file in the same way —
/// they differ only in which recorded calls they then care about.
pub async fn evaluate_user_config() -> hyprforge_lua_import::ImportResult {
    let hypr_dir = hyprforge_core::paths::hypr_config_dir();
    tokio::task::spawn_blocking(move || hyprforge_lua_import::evaluate(&hypr_dir))
        .await
        .unwrap_or_default()
}

/// One lock for every test that repoints `$XDG_CONFIG_HOME`.
///
/// It has to be shared across modules, not one per module: the variable is
/// process-global, so two module-local locks don't exclude each other and a
/// `window_rules` test can retarget the config directory out from under a
/// `shortcuts` test mid-assertion. That produced a genuinely confusing
/// failure — a save reporting success while the file it wrote was nowhere to
/// be found, because it had landed in the other test's temp directory.
#[cfg(test)]
pub(crate) static CONFIG_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Runs `f` with `XDG_CONFIG_HOME` **and** the greeter's export
/// directory pointed at a fresh temporary directory, restoring both
/// afterwards.
///
/// Both, not just the config home. A save in several of these modules
/// reaches `look::republish`, which writes the greeter's exported theme
/// to an absolute path `XDG_CONFIG_HOME` does not redirect — so
/// isolating only the config home still let a test overwrite the real
/// login screen's appearance with a theme derived from an empty temp
/// directory. That is the "never do this to the machine you are working
/// on" rule, reached through a test rather than through the app.
///
/// It lives here because the helper had been copied into eight test
/// modules, and three of those copies isolated only the config home.
/// None of those three can republish today, so nothing was broken — but
/// the next module to copy one would have inherited the gap, and the
/// hazard is invisible until it overwrites something real. There is now
/// one version, and it is the safe one.
///
/// The lock is [`CONFIG_ENV_LOCK`]: the environment is process-global,
/// so two tests doing this at once would each see the other's
/// directory.
#[cfg(test)]
pub(crate) fn with_temp_env<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
    let _lock = CONFIG_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("a temp dir for the test's config home");
    let previous_config = std::env::var_os("XDG_CONFIG_HOME");
    let previous_greet = std::env::var_os(hyprforge_look::theme::EXPORT_DIR_ENV);
    // SAFETY: the lock above is what makes this sound — nothing else in
    // this process reads or writes the environment while it is held.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", dir.path());
        std::env::set_var(hyprforge_look::theme::EXPORT_DIR_ENV, dir.path().join("greet"));
    }
    let out = f(dir.path());
    unsafe {
        restore("XDG_CONFIG_HOME", previous_config);
        restore(hyprforge_look::theme::EXPORT_DIR_ENV, previous_greet);
    }
    out
}

/// Puts one variable back as it was, unset included.
///
/// # Safety
/// The caller holds [`CONFIG_ENV_LOCK`].
#[cfg(test)]
unsafe fn restore(name: &str, previous: Option<std::ffi::OsString>) {
    match previous {
        Some(value) => unsafe { std::env::set_var(name, value) },
        None => unsafe { std::env::remove_var(name) },
    }
}
