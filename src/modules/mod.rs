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
    let config = hypr_dir.join("hyprland.lua");
    bounded_evaluation(
        move || hyprforge_lua_import::evaluate(&hypr_dir),
        config,
        EVALUATION_TIMEOUT,
    )
    .await
}

/// How long an import may take before it is reported as stuck. A real
/// config evaluates in milliseconds.
const EVALUATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Runs `evaluate` on the blocking pool, giving up after `limit`.
///
/// Without the bound, a config the evaluator never finishes (a `require`
/// of something that blocks on open, say) left every import screen on
/// "Reading your config…" for good. Giving up does not stop the thread —
/// a blocking call cannot be cancelled from outside, so it stays stuck
/// until it returns, if ever; bounding that is the evaluator's job. What
/// this guarantees is that the screen hears about it.
///
/// Neither a timeout nor a panic is "nothing to import": both come back
/// as a failure against `config`, which the import review lists, rather
/// than as an empty result that looks like a config with nothing in it.
async fn bounded_evaluation(
    evaluate: impl FnOnce() -> hyprforge_lua_import::ImportResult + Send + 'static,
    config: std::path::PathBuf,
    limit: std::time::Duration,
) -> hyprforge_lua_import::ImportResult {
    let reason = match tokio::time::timeout(limit, tokio::task::spawn_blocking(evaluate)).await {
        Ok(Ok(result)) => return result,
        Ok(Err(e)) => format!("reading it stopped unexpectedly ({e})"),
        Err(_) => format!(
            "reading it took longer than {} seconds, so it was abandoned",
            limit.as_secs()
        ),
    };
    hyprforge_lua_import::ImportResult {
        calls: Vec::new(),
        failures: vec![(config, reason)],
    }
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

#[cfg(test)]
mod evaluation_tests {
    use super::*;

    /// An evaluation that never finishes must still end the import, as a
    /// reported failure — never a screen left waiting for ever.
    #[tokio::test]
    async fn an_evaluation_that_never_finishes_is_reported_not_waited_on() {
        let result = bounded_evaluation(
            || {
                // Long against the limit, short enough that the test
                // runtime's wait for this thread on shutdown stays cheap.
                std::thread::sleep(std::time::Duration::from_secs(1));
                hyprforge_lua_import::ImportResult::default()
            },
            "/cfg/hyprland.lua".into(),
            std::time::Duration::from_millis(50),
        )
        .await;
        assert!(result.calls.is_empty());
        assert_eq!(result.failures.len(), 1, "the user must be told it was abandoned");
    }

    /// A panic inside the evaluator is a failure to report, not an empty
    /// config.
    #[tokio::test]
    async fn an_evaluation_that_panics_is_reported_not_shown_as_empty() {
        let result = bounded_evaluation(
            || panic!("evaluator bug"),
            "/cfg/hyprland.lua".into(),
            std::time::Duration::from_secs(5),
        )
        .await;
        assert_eq!(result.failures.len(), 1);
    }
}
