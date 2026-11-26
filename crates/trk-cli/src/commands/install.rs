use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;
use trk_core::error::{Result, bad_args};
use trk_core::seed;

use crate::Global;
use crate::env::{self, Env};
use crate::render::Out;
use crate::{herdr_ui, launcher, skill};

pub(super) fn run(g: &Global, cmd: crate::Cmd) -> Result<Out> {
    let crate::Cmd::Install {
        force,
        prefix,
        minimal,
        preset,
        from,
        list_presets,
    } = cmd
    else {
        unreachable!()
    };
    if list_presets {
        return Ok(Out::Text(
            seed::PRESETS.join("\n"),
            json!({"presets": seed::PRESETS}),
        ));
    }
    install(
        g,
        force,
        prefix,
        preset.as_deref().unwrap_or("local"),
        from.as_deref(),
        minimal,
    )
}

/// The default issue id prefix for a project directory: its name, reduced to
/// lowercase letters and digits.
fn default_prefix(root: &std::path::Path) -> String {
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let slug: String = name
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect();
    let slug = slug.trim_start_matches(|c: char| c.is_ascii_digit());
    if slug.is_empty() {
        "trk".into()
    } else {
        slug.to_string()
    }
}

type InstallFiles = BTreeMap<PathBuf, Cow<'static, [u8]>>;

fn read_install_files(root: &Path, dir: &Path, files: &mut InstallFiles) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| bad_args(format!("cannot read install source {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            read_install_files(root, &path, files)?;
        } else if kind.is_file() {
            files.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                Cow::Owned(std::fs::read(&path)?),
            );
        } else {
            return Err(bad_args(format!(
                "install source entries must be regular files or directories: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct DetectedProfiles {
    launchers: Vec<String>,
    harnesses: Vec<String>,
}

/// Filter the built-in seed without reserializing its comments and templates.
fn detect_seed_profiles(config: &mut String) -> DetectedProfiles {
    let source: toml::Table = toml::from_str(config).expect("rendered seed is valid TOML");
    let mut detected = DetectedProfiles {
        launchers: Vec::new(),
        harnesses: Vec::new(),
    };
    for (section, key, found) in [
        ("launcher", "use", &mut detected.launchers),
        ("harness", "prefer", &mut detected.harnesses),
    ] {
        let candidates = &source[section][key];
        for name in candidates.as_array().expect("seed preference is an array") {
            let name = name.as_str().expect("seed profile name is a string");
            let profile = &source[section][name];
            let available = if section == "launcher" {
                let profile: launcher::Launcher =
                    profile.clone().try_into().expect("valid seeded launcher");
                profile.is_available()
            } else {
                trk_agent::harness::installed(
                    profile["command"].as_str().expect("seed harness command"),
                )
            };
            if available {
                found.push(name.to_owned());
            }
        }
        let value = toml::Value::Array(found.iter().cloned().map(toml::Value::String).collect());
        *config = config.replace(
            &format!("{key} = {candidates}"),
            &format!("{key} = {value}"),
        );
    }
    detected
}

/// Install the selected source and database, and refresh the agent skill and
/// installed Herdr mod. Existing config files are kept unless `force`.
pub(super) fn install(
    g: &Global,
    force: bool,
    prefix: Option<String>,
    preset: &str,
    from: Option<&Path>,
    minimal: bool,
) -> Result<Out> {
    let mut files: InstallFiles = if minimal {
        BTreeMap::from([
            (PathBuf::from(".gitignore"), Cow::Borrowed(seed::GITIGNORE)),
            (
                PathBuf::from("config.toml"),
                Cow::Borrowed(
                    b"[user]\n{{EMAIL_LINE}}\n\n[issues]\nprefix = \"{{PREFIX}}\"\n\n[run]\nbase = {{BASE}}\n"
                        .as_slice(),
                ),
            ),
        ])
    } else if let Some(root) = from {
        let mut files = BTreeMap::new();
        read_install_files(root, root, &mut files)?;
        files
    } else {
        seed::preset(preset)
            .ok_or_else(|| {
                bad_args(format!("unknown install preset {preset:?}"))
                    .with_hint("run `trk install --list-presets` to see available presets")
            })?
            .into_iter()
            .map(|(path, content)| (PathBuf::from(path), Cow::Borrowed(content)))
            .collect()
    };
    let landing = if minimal || from.is_some() {
        "merge"
    } else {
        seed::landing(preset).expect("built-in preset has a landing recipe")
    };
    let loc = env::Location::find(g);
    let dir = loc.dir.clone();
    let root = loc.root.clone();
    let config_path = dir.join("config.toml");
    if let Some(p) = &prefix {
        trk_core::store::check_prefix(p)?;
        if config_path.exists() && !force {
            return Err(bad_args(format!(
                "{} already exists, so --prefix has no effect",
                config_path.display()
            ))
            .with_hint("edit [issues] prefix in it, or pass --force to rewrite the config"));
        }
    }
    std::fs::create_dir_all(&dir)?;
    let email = g
        .actor
        .as_deref()
        .and_then(|a| a.strip_prefix("human:"))
        .map(String::from)
        .or_else(|| env::git(&root, &["config", "user.email"]).filter(|e| !e.is_empty()));
    let prefix = prefix.unwrap_or_else(|| default_prefix(&root));
    let mut detected = None;
    if let Some(content) = files.get_mut(Path::new("config.toml")) {
        let template = std::str::from_utf8(content)
            .map_err(|e| bad_args(format!("install source config.toml is not UTF-8: {e}")))?;
        let mut rendered = template
            .replace(
                "{{EMAIL_LINE}}",
                &match &email {
                    Some(e) => format!("email = {}", toml::Value::String(e.clone())),
                    None => "# email = \"you@example.com\"".to_string(),
                },
            )
            .replace("{{PREFIX}}", &prefix)
            .replace("{{DEFAULT_TYPE}}", seed::DEFAULT_TYPE)
            .replace(
                "{{BASE}}",
                &toml::Value::String(env::detect_base(&root)).to_string(),
            )
            .replace(
                "{{LANDING}}",
                &toml::Value::String(landing.to_owned()).to_string(),
            );
        if !minimal && from.is_none() && (!config_path.exists() || force) {
            detected = Some(detect_seed_profiles(&mut rendered));
        }
        *content = Cow::Owned(rendered.into_bytes());
    }
    let mut written = vec![];
    let mut kept = vec![];
    // Compare kept files with the selected source, not another preset.
    let mut differ = vec![];
    for (rel, content) in files {
        let p = dir.join(&rel);
        let name = rel.to_string_lossy().into_owned();
        if p.exists() && !force {
            kept.push(name.clone());
            if std::fs::read(&p).is_ok_and(|installed| {
                if rel == Path::new("config.toml") {
                    // Compare profiles, not project identity or formatting.
                    let parse = |bytes: &[u8]| {
                        std::str::from_utf8(bytes)
                            .ok()
                            .and_then(|s| toml::from_str::<toml::Table>(s).ok())
                    };
                    match (parse(&installed), parse(&content)) {
                        (Some(mut installed), Some(mut source)) => {
                            if from.is_none() {
                                // Install-time preferences are machine-specific, not profile drift.
                                for config in [&mut installed, &mut source] {
                                    if let Some(harness) = config
                                        .get_mut("harness")
                                        .and_then(toml::Value::as_table_mut)
                                    {
                                        harness.remove("prefer");
                                    }
                                }
                            }
                            installed.get("harness") != source.get("harness")
                        }
                        _ => true,
                    }
                } else {
                    installed != content.as_ref()
                }
            }) {
                differ.push(name);
            }
            continue;
        }
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, content)?;
        written.push(name);
    }
    let env = Env::at(loc, g)?;
    let created = !env.db.is_file();
    trk_core::Store::install(&env.db, env.store_opts()?)?;
    let skill = skill::install(&root, env.checkout().is_some(), &env.user.skill)?;
    let herdr = herdr_ui::refresh(&herdr_ui::app_home())?;

    let mut text = format!(
        "project   {}\nconfig    {}\n          wrote {} files{}\ndatabase  {}{}\nprefix    {}\nskill     {}",
        root.display(),
        dir.display(),
        written.len(),
        if kept.is_empty() {
            String::new()
        } else {
            format!(", kept {} existing (use --force to overwrite)", kept.len())
        },
        env.db.display(),
        if created { " (created)" } else { "" },
        env.user.issues.prefix,
        skill.lines.join("\n          "),
    );
    if let Some((line, _)) = &herdr {
        text.push_str(&format!("\nherdr     {line}"));
    }
    if let Some(detected) = &detected {
        for (label, names) in [
            ("launchers", &detected.launchers),
            ("harnesses", &detected.harnesses),
        ] {
            text.push_str(&format!(
                "\n{label:10}{}",
                if names.is_empty() {
                    "none detected".to_owned()
                } else {
                    names.join(" > ")
                }
            ));
        }
    }
    if !differ.is_empty() {
        text.push_str(&format!(
            "\n\nKept files that differ from the selected install source (your edits, or newer defaults):\n  {}",
            differ.join("\n  ")
        ));
    }
    let mut warnings = skill.warnings.clone();
    if !minimal && from.is_none() {
        match env.user.run.landing.as_str() {
            "merge" if env.user.check.verify.trim().is_empty() => warnings.push(
                "local landing has no review or approval gate and [check] verify is empty: merges will skip verification; configure it before running agents".to_owned(),
            ),
            "pr" => {
                warnings.push(
                    "GitHub landing requires authenticated `gh`; run `gh auth login` before running agents".to_owned(),
                );
                if !trk_agent::harness::installed("gh") {
                    warnings.push(
                        "`gh` is not on PATH: install the GitHub CLI for pull-request landing".to_owned(),
                    );
                }
            }
            _ => {}
        }
    }
    if let Some(detected) = &detected {
        if detected.launchers.is_empty() {
            warnings.push(
                "no launcher detected: install Herdr, hc, or tmux and edit [launcher] use in config.toml, or run `trk run --fg`".to_owned(),
            );
        }
        if detected.harnesses.is_empty() {
            warnings.push(
                "no harness detected: install omp, claude, codex, opencode, or pi and edit [harness] prefer in config.toml".to_owned(),
            );
        }
    }
    if env.actor.is_none() {
        warnings.push(format!(
            "no email found: set [user] email in {}",
            config_path.display()
        ));
    }
    for w in &warnings {
        text.push_str(&format!("\nwarning: {w}"));
    }
    text.push_str(
        "\n\nNext:\n  trk create \"…\" -d \"…\"\n  trk run                          (start an agent on the first ready issue)\n  trk check --watch                (in a spare terminal: runs waiting checks)",
    );
    Ok(Out::Text(
        text,
        json!({"root": root, "dir": dir, "db": env.db, "db_created": created,
               "prefix": env.user.issues.prefix, "written": written, "kept": kept,
               "differ": differ, "detected": detected, "skill": skill.data,
               "herdr": herdr.map(|(_, data)| data), "warnings": warnings}),
    ))
}
