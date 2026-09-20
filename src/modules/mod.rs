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
