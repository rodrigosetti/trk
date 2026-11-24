//! Build-time embedded configuration, independent of the source checkout.

use std::collections::BTreeMap;

include!(concat!(env!("OUT_DIR"), "/seed.rs"));

pub const CONFIG_TOML: &str = include_str!("../../../seed/config.toml");

/// Built-in fallback for projects whose config predates `[issues] default_type`.
pub const DEFAULT_TYPE: &str = include_str!("../../../seed/default_type.txt").trim_ascii();

/// Installed ignore rules live outside the seed tree so they do not hide new
/// seed files from Git.
pub const GITIGNORE: &[u8] = b"# Written by `trk install`: nothing in .trk/ is tracked.\n*\n";

/// Landing recipe selected by a built-in preset; metadata is not installed.
pub fn landing(name: &str) -> Option<&'static str> {
    TREE.iter().find_map(|(path, content)| {
        let (preset, relative) = path.strip_prefix("presets/")?.split_once('/')?;
        (preset == name && relative == "landing.txt").then(|| {
            std::str::from_utf8(content)
                .expect("UTF-8 landing recipe")
                .trim()
        })
    })
}

/// Common files with the named preset layered over them. Preset paths are
/// relative to the install directory, not to `seed/presets/`.
pub fn preset(name: &str) -> Option<BTreeMap<&'static str, &'static [u8]>> {
    if !PRESETS.contains(&name) {
        return None;
    }
    let mut files: BTreeMap<_, _> = TREE
        .iter()
        .copied()
        .filter(|(path, _)| !path.starts_with("presets/") && *path != "default_type.txt")
        .collect();
    files.insert(".gitignore", GITIGNORE);
    for &(path, content) in TREE {
        if let Some((preset, relative)) = path
            .strip_prefix("presets/")
            .and_then(|p| p.split_once('/'))
            && preset == name
            && relative != "landing.txt"
        {
            files.insert(relative, content);
        }
    }
    Some(files)
}
