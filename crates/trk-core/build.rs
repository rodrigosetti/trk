//! Embed every seed file, including additions, without a Rust file manifest.

use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::{Path, PathBuf};

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read seed directory") {
        let entry = entry.expect("read seed entry");
        let kind = entry.file_type().expect("read seed file type");
        if kind.is_dir() {
            collect(&entry.path(), files);
        } else if kind.is_file() {
            files.push(entry.path());
        } else {
            panic!(
                "seed entries must be regular files or directories: {}",
                entry.path().display()
            );
        }
    }
}

fn main() {
    let root = Path::new(&std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../../seed")
        .canonicalize()
        .expect("find seed directory");
    println!("cargo:rerun-if-changed={}", root.display());
    println!("cargo:rerun-if-changed=build.rs");
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    let mut presets = BTreeSet::new();
    let mut source = String::from("const TREE: &[(&str, &[u8])] = &[\n");
    for path in files {
        let relative = path
            .strip_prefix(&root)
            .unwrap()
            .to_str()
            .expect("UTF-8 seed path");
        if let Some((name, _)) = relative
            .strip_prefix("presets/")
            .and_then(|p| p.split_once('/'))
        {
            presets.insert(name.to_owned());
        }
        writeln!(
            source,
            "    ({relative:?}, include_bytes!({:?})),",
            path.to_str().unwrap()
        )
        .unwrap();
    }
    source.push_str("];\npub const PRESETS: &[&str] = &[");
    for name in presets {
        write!(source, "{name:?},").unwrap();
    }
    source.push_str("];\n");
    std::fs::write(
        Path::new(&std::env::var_os("OUT_DIR").unwrap()).join("seed.rs"),
        source,
    )
    .expect("write embedded seed manifest");
}
