//! The Herdr mod that shows the dashboard inside the Herdr app, installed by
//! `trk install` when Herdr's folder exists. Its files are embedded in the
//! binary, so the mod always matches the `trk --json` it reads.
//!
//! The mod folder (`mods/trk/`) is trk's: it is rewritten on every install.
//! The layout (`layouts/trk.json`) is the person's once written: it is kept
//! unless `force`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use trk_core::error::Result;

use crate::env::home;

pub const MOD: &str = "trk";

/// The mod's files, relative to `mods/trk/`.
const MOD_FILES: &[(&str, &str)] = &[
    ("main.js", include_str!("../../../herdr/mods/trk/main.js")),
    ("mod.json", include_str!("../../../herdr/mods/trk/mod.json")),
    (
        "spinner.svg",
        include_str!("../../../herdr/mods/trk/spinner.svg"),
    ),
];

/// The layout's files, relative to `layouts/`.
const LAYOUT_FILES: &[(&str, &str)] = &[
    ("trk.json", include_str!("../../../herdr/layouts/trk.json")),
    ("trk.svg", include_str!("../../../herdr/layouts/trk.svg")),
];

/// Herdr's folder: the app's whole interface, as files.
pub fn app_home() -> PathBuf {
    home().join(".config/herdr-app")
}

/// What installing the mod did.
pub struct Installed {
    pub lines: Vec<String>,
    pub data: Value,
}

fn same(path: &Path, content: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|s| s == content)
}

/// Install or refresh the mod and its layout in `app` (Herdr's folder).
/// `None` when Herdr is not set up on this machine.
pub fn install(app: &Path, force: bool) -> Result<Option<Installed>> {
    if !app.join("mods").is_dir() {
        return Ok(None);
    }
    let mut lines = vec![];
    let dir = app.join("mods").join(MOD);
    let mod_state = if dir.is_symlink() {
        lines.push(format!(
            "{} is a symlink (a development copy?); left alone",
            dir.display()
        ));
        "linked"
    } else {
        let existed = dir.join("main.js").exists();
        std::fs::create_dir_all(&dir)?;
        // Only what changed: Herdr reloads the mod on every save.
        let mut changed = 0;
        for (rel, content) in MOD_FILES {
            let p = dir.join(rel);
            if !same(&p, content) {
                std::fs::write(&p, content)?;
                changed += 1;
            }
        }
        lines.push(match (existed, changed) {
            (true, 0) => format!("mod already current at {}", dir.display()),
            (true, _) => format!("mod updated at {}", dir.display()),
            (false, _) => format!("mod installed at {}", dir.display()),
        });
        "current"
    };
    let layouts = app.join("layouts");
    let layout = layouts.join("trk.json");
    let layout_state = if layout.exists() && !force {
        if !same(&layout, LAYOUT_FILES[0].1) {
            lines.push(format!(
                "layout kept at {} (differs from this trk's; --force to overwrite)",
                layout.display()
            ));
            "kept"
        } else {
            "current"
        }
    } else {
        std::fs::create_dir_all(&layouts)?;
        for (rel, content) in LAYOUT_FILES {
            std::fs::write(layouts.join(rel), content)?;
        }
        lines.push(format!("layout written at {}", layout.display()));
        "written"
    };
    lines.push(
        "open it: herdr-app do pane.split --direction right --mod trk   (or switch to the trk layout)"
            .into(),
    );
    Ok(Some(Installed {
        lines,
        data: json!({"mod": dir, "mod_state": mod_state, "layout": layout, "layout_state": layout_state}),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_written_without_herdr() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(install(tmp.path(), false).unwrap().is_none());
        assert!(!tmp.path().join("mods").exists());
    }

    #[test]
    fn the_mod_is_refreshed_and_an_edited_layout_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path();
        std::fs::create_dir_all(app.join("mods")).unwrap();

        let first = install(app, false).unwrap().unwrap();
        assert_eq!(first.data["layout_state"], "written");
        for (rel, content) in MOD_FILES {
            assert!(same(&app.join("mods/trk").join(rel), content), "{rel}");
        }
        // Herdr's own files in the mod folder survive a refresh.
        std::fs::write(app.join("mods/trk/jsconfig.json"), "{}").unwrap();
        std::fs::write(app.join("mods/trk/main.js"), "stale").unwrap();
        std::fs::write(app.join("layouts/trk.json"), "{\"mine\": true}").unwrap();

        let again = install(app, false).unwrap().unwrap();
        assert_eq!(again.data["layout_state"], "kept");
        assert!(same(&app.join("mods/trk/main.js"), MOD_FILES[0].1));
        assert!(app.join("mods/trk/jsconfig.json").exists());
        assert!(same(&app.join("layouts/trk.json"), "{\"mine\": true}"));

        let forced = install(app, true).unwrap().unwrap();
        assert_eq!(forced.data["layout_state"], "written");
        assert!(same(&app.join("layouts/trk.json"), LAYOUT_FILES[0].1));
    }
}
