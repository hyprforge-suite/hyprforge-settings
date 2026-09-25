//! Behaviour and platform settings — `misc`, `binds`, `xwayland`,
//! `render`, `opengl`, `ecosystem` and `quirks`.
//!
//! The screen is [`super::catalog_screen`], shared with Input. Nothing
//! here is discoverable from the machine, so it offers no pickers beyond
//! what the catalogue itself declares.

use super::catalog_screen::{Catalogued, CatalogScreen};
use hyprforge_core::hlconfig::{Catalog, Settings};
use hyprforge_core::lua_setup::{Placement, SetupError, SetupPlan};
use hyprforge_system::catalog::CATALOG;
use std::path::{Path, PathBuf};

pub type SystemModule = CatalogScreen<System>;

pub struct System;

impl Catalogued for System {
    const SUBJECT: &'static str = "system settings";
    const STORE: &'static str = "system.toml";

    fn catalog() -> &'static Catalog {
        &CATALOG
    }

    fn require_line() -> &'static str {
        hyprforge_system::setup::REQUIRE_LINE
    }

    fn placement() -> Placement {
        hyprforge_system::setup::PLACEMENT
    }

    fn toml_path() -> PathBuf {
        hyprforge_core::paths::hyprforge_config_dir().join("system.toml")
    }

    fn lua_path() -> PathBuf {
        hyprforge_core::paths::hypr_hyprforge_dir().join("system.lua")
    }

    fn generate(settings: &Settings) -> String {
        hyprforge_system::apply::generate(settings)
    }

    fn apply(lua_path: &Path, settings: &Settings) -> Result<(), String> {
        hyprforge_system::apply::apply(lua_path, settings).map_err(|e| e.to_string())
    }

    fn install(hyprland_lua: &Path) -> Result<SetupPlan, SetupError> {
        hyprforge_system::setup::install(hyprland_lua)
    }
}
