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
/// save on the Appearance page. The greeter's copy still waits for that
/// save — see the note on the write.
///
/// No test of its own: it is glue around `publish`, which writes the
/// real paths and must never be run from a test. Returns whether it
/// wrote.
pub fn republish_if_stale() -> bool {
    let mut theme = hyprforge_appearance::look::resolve();
    theme.wallpaper = wallpaper();
    if hyprforge_look::Theme::load(&hyprforge_paths::lock_toml_path()).is_ok_and(|current| current == theme) {
        return false;
    }
    // The lock screen's copy only, not the greeter's export: that one is
    // a system directory this user can write, and Settings is run by hand
    // against an isolated config, where doing this on every start would
    // put a test's look on the real login screen — which happened once.
    // The greeter's copy still follows an explicit save (`republish`).
    if let Err(e) = theme.save(&hyprforge_paths::lock_toml_path()) {
        tracing::warn!(error = %e, "couldn't bring the lock screen's look up to date");
    }
    true
}

/// Where the downscaled copy lives. Derived, and regenerated whenever
/// the original is newer.
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

    // Skip the work when the copy is already current. Comparing mtimes
    // rather than hashing: this runs on a settings save, and reading 36
    // megapixels to decide whether to read 36 megapixels is silly.
    let up_to_date = || -> Option<bool> {
        let source = std::fs::metadata(original).ok()?.modified().ok()?;
        let derived = std::fs::metadata(scaled).ok()?.modified().ok()?;
        Some(derived >= source)
    };
    if up_to_date().unwrap_or(false) {
        return Some(scaled.to_path_buf());
    }

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
    tracing::debug!(
        from = %format!("{width}x{height}"),
        to = %format!("{}x{}", resized.width(), resized.height()),
        "scaled the wallpaper for the auth screens"
    );
    Some(scaled.to_path_buf())
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
