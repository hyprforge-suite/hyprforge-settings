//! Publishing the shared look, from the one place that can see all of it.
//!
//! The colours and fonts come from `hyprforge-appearance` and the
//! wallpaper from `hyprforge-ecosystem`, and neither crate should depend
//! on the other to fetch half a theme. The Settings app already depends
//! on both, so the assembly happens here.
//!
//! Called from **both** screens that can change what the auth screens
//! look like: Appearance owns the colours, Desktop owns the wallpaper.
//! Wiring only one of them is how the lock screen ends up showing last
//! week's wallpaper.

use std::path::{Path, PathBuf};

/// The longest edge the auth screens' wallpaper is allowed to have.
///
/// Not about looks — about what the lock screen has to allocate. A
/// 8001x4501 photograph is 36 megapixels, which decodes to 144MB and
/// peaks near 300MB once the renderer has its own premultiplied copy.
/// That is a lot to ask of the one process on the machine that must not
/// fail, and the failure is not graceful: an allocation failure during
/// decode is exactly the case that poisons `iced_tiny_skia`'s image
/// cache and panics on the next frame, which on a lock screen means a
/// machine you need another TTY to get into.
///
/// 4K's long edge, so anything up to a 4K display is still shown at
/// full resolution and nothing larger is ever decoded.
const MAX_WALLPAPER_EDGE: u32 = 3840;

fn wallpaper_toml() -> PathBuf {
    hyprforge_paths::hyprforge_config_dir().join("wallpaper.toml")
}

/// Resolves the look and writes it where the lock screen and the greeter
/// read it.
///
/// Never fails loudly: the settings the user was actually saving are
/// already on disk by the time this runs, and failing their save because
/// a lock screen theme could not be written would be reporting the wrong
/// problem.
pub fn republish() {
    let mut theme = hyprforge_appearance::look::resolve();
    theme.wallpaper = wallpaper();

    if let Err(e) = hyprforge_appearance::look::publish(&theme) {
        tracing::warn!(error = %e, "couldn't publish the look for the lock screen");
    }
}

/// [`republish`], but only when the copy the lock screen reads is not
/// what resolving would write now — called as Settings starts.
///
/// The look follows things that change without Settings hearing: the
/// border colours a person edits in `hyprland.lua` by hand, which is
/// where most configs keep them. Every app resolves those as it opens;
/// the lock screen and the greeter read a published copy instead (asking
/// Hyprland before a lock can appear would put another process's answer
/// in front of it), so without this they kept the look from the last
/// save on the Appearance page. The greeter's copy is brought up to date
/// too, but only from the user's own config — see [`may_export_on_start`].
///
/// No test of its own: it is glue around writes to the real paths, which
/// must never be run from a test; the decision it turns on is tested.
/// Returns whether it wrote.
pub fn republish_if_stale() -> bool {
    let mut theme = hyprforge_appearance::look::resolve();
    theme.wallpaper = wallpaper();
    let mut wrote = false;
    if !hyprforge_look::Theme::load(&hyprforge_paths::lock_toml_path()).is_ok_and(|current| current == theme) {
        if let Err(e) = theme.save(&hyprforge_paths::lock_toml_path()) {
            tracing::warn!(error = %e, "couldn't bring the lock screen's look up to date");
        }
        wrote = true;
    }

    let dir = hyprforge_look::theme::export_dir();
    let own = may_export_on_start(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
        std::env::var_os(hyprforge_look::theme::EXPORT_DIR_ENV).is_some_and(|v| !v.is_empty()),
    );
    // No directory is no greeter installed: nothing to bring up to date,
    // and nothing to warn about on every start. (An explicit save still
    // warns, through `publish` — there the user asked for the write.)
    if own && dir.is_dir() {
        let exported = hyprforge_look::Theme::load(&dir.join("theme.toml"));
        if !exported.is_ok_and(|exported| same_look(&exported, &theme)) {
            if let Err(e) = theme.export(&dir) {
                tracing::warn!(error = %e, dir = %dir.display(), "couldn't bring the greeter's look up to date");
            }
            wrote = true;
        }
    }
    wrote
}

/// Whether Settings, as it starts, may rewrite the greeter's export.
///
/// That export is a system directory this user can write, shared by the
/// login screen, and Settings is often run by hand against an isolated
/// `XDG_CONFIG_HOME` (CLAUDE.md asks for exactly that). A start that
/// wrote from there put a test's look on the real login screen once,
/// which is why the greeter used to follow only an explicit save — and
/// so kept a border edited by hand in `hyprland.lua` until the next one.
///
/// So: yes when the export directory was named on purpose
/// (`HYPRFORGE_GREET_DIR`, which is how a test points it at a scratch
/// copy), or when the config in use is the user's own — the one
/// `hyprforge_paths::config_home` falls back to, `$HOME/.config`. A run
/// whose config is anywhere else is a test, or someone who keeps it
/// elsewhere on purpose; either way an explicit save on Appearance still
/// exports, and only the automatic write at start is held back.
fn may_export_on_start(xdg: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>, export_dir_named: bool) -> bool {
    if export_dir_named {
        return true;
    }
    // Unset, empty or relative: `config_home` ignores it (a relative value
    // is invalid per the XDG spec) and uses the default — the user's own.
    let Some(xdg) = xdg.map(PathBuf::from).filter(|p| p.is_absolute()) else {
        return true;
    };
    let Some(home) = home.map(PathBuf::from).filter(|p| p.is_absolute()) else {
        // No `$HOME` to compare against: not provably the user's own.
        return false;
    };
    let default = home.join(".config");
    // Canonical where both resolve, so `~/.config` through a symlink, or
    // spelled with a trailing slash, is still recognised.
    match (xdg.canonicalize(), default.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => xdg == default,
    }
}

/// Whether the greeter's export already shows `theme`. The export's
/// wallpaper is its own copy, so its path never equals the original;
/// whether there is one is what can be compared, and a changed wallpaper
/// is exported by the save that changed it (`republish`, from Desktop).
fn same_look(exported: &hyprforge_look::Theme, theme: &hyprforge_look::Theme) -> bool {
    exported.wallpaper.is_some() == theme.wallpaper.is_some()
        && hyprforge_look::Theme { wallpaper: None, ..exported.clone() } == hyprforge_look::Theme { wallpaper: None, ..theme.clone() }
}

/// Where the downscaled copy lives. Derived, and regenerated whenever
/// the source it was made from changes (see `source_marker`).
fn scaled_wallpaper_path() -> PathBuf {
    hyprforge_paths::hyprforge_config_dir().join("lock-wallpaper.png")
}

/// A copy of `original` no larger than [`MAX_WALLPAPER_EDGE`], or the
/// original when it is already small enough.
///
/// Done here, once, when settings are saved — rather than on every lock,
/// where it would cost the same allocation every time and where failing
/// is unrecoverable. Failure here is free: the caller keeps the original,
/// which is what would have been used anyway.
fn scaled_wallpaper(original: &Path) -> Option<PathBuf> {
    scale_into(original, &scaled_wallpaper_path())
}

/// The scaling itself, with the destination passed in so it can be
/// tested without writing into the real config directory.
fn scale_into(original: &Path, scaled: &Path) -> Option<PathBuf> {
    let reader = image::ImageReader::open(original)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    if width.max(height) <= MAX_WALLPAPER_EDGE {
        return None;
    }

    // Skip the work when the copy is already current. Comparing a
    // recorded identity rather than hashing: this runs on a settings
    // save, and reading 36 megapixels to decide whether to read 36
    // megapixels is silly.
    //
    // The identity is the source's path, size and mtime, kept in a
    // sidecar beside the copy — not "the copy is newer than the source".
    // The copy has one fixed name whatever it was made from, so that test
    // called last week's copy current for any *older* wallpaper the user
    // switched to, and the lock screen kept the previous picture.
    let marker = source_marker(scaled);
    let identity = source_identity(original);
    if identity.is_some()
        && scaled.exists()
        && std::fs::read_to_string(&marker).ok() == identity
    {
        return Some(scaled.to_path_buf());
    }
    // Gone before the copy is replaced: a crash between the rename below
    // and the new marker must leave no marker at all, never one naming a
    // different source than the copy beside it.
    let _ = std::fs::remove_file(&marker);

    let image = image::ImageReader::open(original).ok()?.with_guessed_format().ok()?;
    let image = image.decode().ok()?;
    // Lanczos3 because this happens once and the result is looked at
    // every time the machine is locked.
    let resized = image.resize(
        MAX_WALLPAPER_EDGE,
        MAX_WALLPAPER_EDGE,
        image::imageops::FilterType::Lanczos3,
    );
    if let Some(parent) = scaled.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }

    // Encoded to a sibling and renamed, never written in place. A save
    // interrupted halfway leaves a truncated PNG whose mtime is *newer*
    // than its source — so `up_to_date` above reports it current, and
    // the broken file is returned forever. Re-saving settings never
    // regenerates it; the auth screens drop an undecodable wallpaper and
    // show a flat background with no error, so the only recovery is
    // knowing to delete this file by hand.
    //
    // The temp name carries the pid for the same reason
    // `hyprforge_paths::write_atomic` does: two Settings instances would
    // otherwise truncate each other's half-encoded image. This cannot
    // use `write_atomic` itself — that takes a `&str`, and this is a
    // PNG encoder writing bytes.
    let tmp = scaled.with_extension(format!("{}.tmp.png", std::process::id()));
    if resized.save(&tmp).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    if std::fs::rename(&tmp, scaled).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    // Only after the copy is in place. Failing to write it costs a
    // re-scale next time, nothing more.
    if let Some(identity) = identity {
        let _ = hyprforge_paths::write_atomic(&marker, &identity);
    }
    tracing::debug!(
        from = %format!("{width}x{height}"),
        to = %format!("{}x{}", resized.width(), resized.height()),
        "scaled the wallpaper for the auth screens"
    );
    Some(scaled.to_path_buf())
}

/// Where the record of what `scaled` was made from lives.
fn source_marker(scaled: &Path) -> PathBuf {
    let mut name = scaled.as_os_str().to_owned();
    name.push(".source");
    PathBuf::from(name)
}

/// What identifies the source a copy was made from: its path, size and
/// modification time, one per line. `None` when it cannot be read, which
/// never matches a marker and so always re-scales.
fn source_identity(original: &Path) -> Option<String> {
    let meta = std::fs::metadata(original).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let path = std::fs::canonicalize(original).unwrap_or_else(|_| original.to_path_buf());
    Some(format!("{}\n{}\n{mtime}\n", path.display(), meta.len()))
}

/// Whether `original` is too large for the auth screens to decode
/// safely.
///
/// Separate from [`scale_into`] because the two answers it used to
/// conflate mean opposite things. `scale_into` returns `None` both for
/// "already small enough" and "scaling failed", and the caller then
/// falls back to the original — which is right for the first and exactly
/// wrong for the second. By the time a scale fails we have *already
/// established* the image is oversized, and `MAX_WALLPAPER_EDGE`'s own
/// doc says what that means: a 36-megapixel decode peaks near 300MB, an
/// allocation failure poisons `iced_tiny_skia`'s image cache, and the
/// next frame panics — on a lock screen, a machine needing another TTY.
///
/// There is no size guard downstream. `authui`'s check is
/// `width > 0 && height > 0`, and `hyprforge-look` does not look at the
/// wallpaper's size at all. This is the only place the cap exists.
fn is_oversized(original: &Path) -> bool {
    let Ok(reader) = image::ImageReader::open(original).and_then(|r| r.with_guessed_format())
    else {
        // Unreadable here is unreadable in the renderer too, which drops
        // it. Not this function's problem to report.
        return false;
    };
    match reader.into_dimensions() {
        Ok((width, height)) => width.max(height) > MAX_WALLPAPER_EDGE,
        Err(_) => false,
    }
}

/// The wallpaper the auth screens should show, if there is one.
fn wallpaper() -> Option<PathBuf> {
    match hyprforge_ecosystem::storage::load::<hyprforge_ecosystem::wallpaper::Settings>(
        &wallpaper_toml(),
    ) {
        Ok(settings) => {
            let chosen = hyprforge_ecosystem::wallpaper::for_auth_screen(&settings)?;
            match scaled_wallpaper(&chosen) {
                Some(scaled) => Some(scaled),
                // Falling back to the original is right only when the
                // image never needed scaling. When it did and the
                // scaling failed, handing the original to the auth
                // screens hands them the exact file the cap exists to
                // keep out — so the wallpaper is dropped instead. A
                // plain background is a visible, recoverable
                // disappointment; a lock screen that panics on its next
                // frame is not.
                None if is_oversized(&chosen) => {
                    tracing::warn!(
                        path = %chosen.display(),
                        "couldn't scale the wallpaper for the auth screens, and it is too \
                         large to use as it is; the lock screen and greeter will show a \
                         plain background"
                    );
                    None
                }
                None => Some(chosen),
            }
        }
        Err(e) => {
            // A missing file is first-run and loads as empty. Anything
            // else means the wallpaper settings exist and could not be
            // read, which is worth saying rather than silently showing a
            // flat background.
            tracing::warn!(error = %e, "couldn't read the wallpaper settings");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<std::ffi::OsString> {
        Some(s.into())
    }

    /// The login screen is shared and lives outside any config directory,
    /// so a run of Settings against a scratch `XDG_CONFIG_HOME` must
    /// never restyle it as it starts — that happened once.
    #[test]
    fn an_isolated_config_never_restyles_the_login_screen_on_start() {
        let home = tempfile::tempdir().unwrap();
        // Both exist, as on a real machine, so the comparison is the one
        // between two directories that resolve — not a missing path.
        std::fs::create_dir(home.path().join(".config")).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let home_str = home.path().to_str().unwrap();
        assert!(!may_export_on_start(os(scratch.path().to_str().unwrap()), os(home_str), false));
    }

    /// The user's own config is the default location, however it is
    /// spelled: unset, empty, relative (invalid, so ignored), or the same
    /// directory named outright — through a symlink included.
    #[test]
    fn the_users_own_config_brings_the_login_screen_up_to_date() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join(".config");
        std::fs::create_dir(&config).unwrap();
        let link = home.path().join("cfg");
        std::os::unix::fs::symlink(&config, &link).unwrap();
        let home_str = home.path().to_str().unwrap();

        assert!(may_export_on_start(None, os(home_str), false), "unset");
        assert!(may_export_on_start(os(""), os(home_str), false), "empty");
        assert!(may_export_on_start(os("relative/dir"), os(home_str), false), "relative");
        assert!(may_export_on_start(os(config.to_str().unwrap()), os(home_str), false), "named outright");
        assert!(may_export_on_start(os(&format!("{}/", config.display())), os(home_str), false), "trailing slash");
        assert!(may_export_on_start(os(link.to_str().unwrap()), os(home_str), false), "through a symlink");
    }

    /// A test that names its own export directory may write it from any
    /// config — that is how the suite's tests keep their hands off
    /// `/var/lib/hyprforge/greet`.
    #[test]
    fn a_named_export_directory_may_always_be_written() {
        let scratch = tempfile::tempdir().unwrap();
        assert!(may_export_on_start(os(scratch.path().to_str().unwrap()), os("/home/nobody"), true));
    }

    /// With no `$HOME` there is nothing to show the config is the user's.
    #[test]
    fn without_a_home_a_named_config_is_not_the_users_own() {
        assert!(!may_export_on_start(os("/somewhere"), None, false));
    }

    /// A border edited by hand changes the accent: that is a stale export.
    /// The export's wallpaper is its own copy, so a different path there
    /// is not.
    #[test]
    fn the_export_is_stale_when_the_colours_move_and_not_when_only_its_wallpaper_copy_differs() {
        let theme = hyprforge_look::Theme { wallpaper: Some("/home/v/pic.png".into()), ..Default::default() };
        let exported = hyprforge_look::Theme { wallpaper: Some("/var/lib/hyprforge/greet/wallpaper.png".into()), ..theme.clone() };
        assert!(same_look(&exported, &theme));

        let moved = hyprforge_look::Theme { accent: hyprforge_look::Color::from_argb_hex("ffff0000").unwrap(), ..theme.clone() };
        assert!(!same_look(&exported, &moved));

        let without = hyprforge_look::Theme { wallpaper: None, ..theme.clone() };
        assert!(!same_look(&exported, &without));
    }

    /// A photograph far larger than any screen must not reach the lock
    /// screen at full size. 36 megapixels peaks near 300MB in the one
    /// process on the machine that must not fail, and an allocation
    /// failure during decode is the case that panics the renderer.
    #[test]
    fn an_oversized_wallpaper_is_scaled_down() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("huge.png");
        // Wider than 4K in one edge, so it has to be reduced.
        image::RgbaImage::from_pixel(5000, 1200, image::Rgba([10, 20, 30, 255]))
            .save(&original)
            .unwrap();

        // Point the derived copy inside the temp dir rather than the
        // real config directory.
        let scaled = dir.path().join("lock-wallpaper.png");
        let out = scale_into(&original, &scaled).expect("an oversized image is scaled");
        assert_eq!(out, scaled);

        let (w, h) = image::ImageReader::open(&scaled)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert!(w.max(h) <= MAX_WALLPAPER_EDGE, "still {w}x{h}");
        // The aspect ratio has to survive, or the lock screen shows a
        // stretched photograph.
        assert!((w as f32 / h as f32 - 5000.0 / 1200.0).abs() < 0.01, "{w}x{h}");
    }

    /// Anything a screen could actually display is left alone — copying
    /// and re-encoding it would cost quality for nothing.
    #[test]
    fn a_reasonable_wallpaper_is_left_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("fine.png");
        image::RgbaImage::from_pixel(2560, 1600, image::Rgba([1, 2, 3, 255]))
            .save(&original)
            .unwrap();
        assert_eq!(scale_into(&original, &dir.path().join("out.png")), None);
    }

    /// Not an image, and nothing to scale. The caller keeps the original,
    /// which the renderer guard will then reject on its own terms.
    #[test]
    fn something_that_is_not_an_image_scales_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("notes.png");
        std::fs::write(&text, b"not a png").unwrap();
        assert_eq!(scale_into(&text, &dir.path().join("out.png")), None);
        assert_eq!(scale_into(&dir.path().join("missing.png"), &dir.path().join("out.png")), None);
    }

    /// Re-scaling 36 megapixels on every settings save would be a
    /// noticeable stall for no gain, so a current copy is reused.
    #[test]
    fn a_current_copy_is_not_regenerated() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("huge.png");
        image::RgbaImage::from_pixel(5000, 1200, image::Rgba([9, 9, 9, 255]))
            .save(&original)
            .unwrap();
        let scaled = dir.path().join("lock-wallpaper.png");

        scale_into(&original, &scaled).unwrap();
        let first = std::fs::metadata(&scaled).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        scale_into(&original, &scaled).unwrap();
        assert_eq!(
            std::fs::metadata(&scaled).unwrap().modified().unwrap(),
            first,
            "the copy was rewritten even though the original had not changed"
        );
    }

    /// The copy has one fixed name whatever it was made from, so "newer
    /// than the source" cannot be the whole freshness test: switching to
    /// an *older* oversized wallpaper found last week's copy newer than it
    /// and kept showing the previous picture on the lock screen.
    #[test]
    fn switching_to_an_older_wallpaper_replaces_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let older = dir.path().join("older.png");
        let newer = dir.path().join("newer.png");
        image::RgbaImage::from_pixel(5000, 1200, image::Rgba([200, 0, 0, 255]))
            .save(&older)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        image::RgbaImage::from_pixel(5000, 1200, image::Rgba([0, 0, 200, 255]))
            .save(&newer)
            .unwrap();
        let scaled = dir.path().join("lock-wallpaper.png");

        scale_into(&newer, &scaled).unwrap();
        scale_into(&older, &scaled).unwrap();
        let pixel = *image::open(&scaled).unwrap().to_rgba8().get_pixel(0, 0);
        assert_eq!(pixel, image::Rgba([200, 0, 0, 255]), "still the previous wallpaper");
    }
}

#[cfg(test)]
mod scaling_failures {
    use super::*;

    fn oversized_png(path: &Path) {
        let edge = MAX_WALLPAPER_EDGE + 10;
        image::RgbImage::new(edge, 8).save(path).unwrap();
    }

    /// The failure that repairs itself never — a truncated copy has a
    /// newer mtime than its source, so the freshness check calls it
    /// current and hands it back forever.
    #[test]
    fn a_half_written_copy_is_never_left_where_the_freshness_check_will_trust_it() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("wall.png");
        oversized_png(&original);

        // A read-only destination directory: `create_dir_all` on an
        // existing directory still succeeds, the size check has already
        // passed, and the encoder is what fails — which is the shape of
        // a full disk or a quota, the realistic cases here.
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let scaled = out.join("scaled.png");
        let mut perms = std::fs::metadata(&out).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o555);
        std::fs::set_permissions(&out, perms.clone()).unwrap();

        assert_eq!(scale_into(&original, &scaled), None);

        let leftovers: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");

        // Let the tempdir clean itself up.
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&out, perms).unwrap();
    }

    #[test]
    fn a_successful_scale_lands_at_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("wall.png");
        oversized_png(&original);
        let scaled = dir.path().join("scaled.png");

        assert_eq!(scale_into(&original, &scaled), Some(scaled.clone()));
        let (w, h) = image::ImageReader::open(&scaled)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert!(w.max(h) <= MAX_WALLPAPER_EDGE, "got {w}x{h}");
    }

    /// The guard must fail closed. Before this, a failed scale returned
    /// `None` and the caller fell back to the original — handing the
    /// renderer the oversized image the cap exists to keep out.
    #[test]
    fn an_image_that_needs_scaling_is_recognised_as_oversized() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.png");
        oversized_png(&big);
        assert!(is_oversized(&big));

        let small = dir.path().join("small.png");
        image::RgbImage::new(64, 64).save(&small).unwrap();
        assert!(!is_oversized(&small), "a small image must still be usable as it is");

        // Not an image at all: the renderer drops it on its own terms,
        // and claiming it is oversized would be inventing a reason.
        let junk = dir.path().join("junk.png");
        std::fs::write(&junk, b"not a png").unwrap();
        assert!(!is_oversized(&junk));
    }
}
