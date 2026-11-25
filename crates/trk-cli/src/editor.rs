//! Interactive editing: `title`, `priority`, `type` and `epic` are TOML front matter
//! between `+++` lines and the description is the markdown body. When
//! validation fails on save, the file reopens with the errors as comments.

use std::io::Write;
use std::process::Command;

use trk_core::error::{Code, Error, Result};

pub fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// The authored fields of an issue, as edited.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Doc {
    pub title: String,
    pub priority: String,
    pub issue_type: String,
    pub epic: String,
    pub description: String,
}

fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

pub fn document(errors: &[String], d: &Doc) -> String {
    let mut s = String::from("+++\n");
    for e in errors {
        s.push_str(&format!("# error: {e}\n"));
    }
    s.push_str(&format!("title    = {}\n", toml_str(&d.title)));
    s.push_str(&format!(
        "priority = {}  # P0 urgent, P1 high, P2 normal, P3 low\n",
        toml_str(&d.priority)
    ));
    s.push_str(&format!("type     = {}\n", toml_str(&d.issue_type)));
    s.push_str(&format!(
        "epic     = {}  # existing active epic; empty clears it\n",
        toml_str(&d.epic)
    ));
    s.push_str("+++\n");
    s.push_str(&d.description);
    if !d.description.ends_with('\n') {
        s.push('\n');
    }
    s
}

pub fn parse(doc: &str) -> Result<Doc> {
    let bad = |m: String| Error::new(Code::BadArgs, m);
    let rest = doc
        .trim_start()
        .strip_prefix("+++")
        .ok_or_else(|| bad("the file must start with a +++ front matter block".into()))?;
    let (front, body) = rest
        .split_once("\n+++")
        .ok_or_else(|| bad("front matter is not closed with +++".into()))?;
    let table: toml::Table =
        toml::from_str(front).map_err(|e| bad(format!("front matter: {e}")))?;
    if !table.contains_key("type") {
        return Err(bad("front matter requires 'type'".into()));
    }
    let mut d = Doc {
        description: body.trim().to_string(),
        ..Default::default()
    };
    for (k, v) in table {
        let v = match v {
            toml::Value::String(s) => s,
            other => other.to_string(),
        };
        match k.as_str() {
            "title" => d.title = v,
            "priority" => d.priority = v,
            "type" => d.issue_type = v,
            "epic" => d.epic = v,
            other => return Err(bad(format!("unknown key '{other}' in the front matter"))),
        }
    }
    Ok(d)
}

/// Open `$EDITOR` on the document until `apply` accepts it. Returns `apply`'s result.
pub fn edit_loop<T>(initial: &Doc, mut apply: impl FnMut(Doc) -> Result<T>) -> Result<T> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let path = std::env::temp_dir().join(format!("trk-{}.md", std::process::id()));
    let mut doc = document(&[], initial);
    loop {
        std::fs::File::create(&path)?.write_all(doc.as_bytes())?;
        let status = Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$1\""))
            .arg("sh")
            .arg(&path)
            .status()?;
        if !status.success() {
            return Err(Error::new(
                Code::BadArgs,
                "editor exited with an error; nothing saved",
            ));
        }
        let edited = std::fs::read_to_string(&path)?;
        if edited.trim().is_empty() {
            let _ = std::fs::remove_file(&path);
            return Err(Error::new(Code::BadArgs, "empty file; nothing saved"));
        }
        // Strip previous error comments before parsing.
        let cleaned: String = edited
            .lines()
            .filter(|l| !l.starts_with("# error:"))
            .collect::<Vec<_>>()
            .join("\n");
        match parse(&cleaned).and_then(&mut apply) {
            Ok(v) => {
                let _ = std::fs::remove_file(&path);
                return Ok(v);
            }
            Err(e) => {
                let mut errors = vec![e.message()];
                if let Some(h) = e.hint() {
                    errors.push(format!("hint: {h}"));
                }
                let notes: String = errors.iter().map(|e| format!("# error: {e}\n")).collect();
                // Keep what the user typed; put the errors where they will be seen.
                doc = match cleaned.trim_start().strip_prefix("+++") {
                    Some(rest) => format!("+++\n{notes}{}\n", rest.trim_start_matches('\n')),
                    None => format!("{notes}{cleaned}\n"),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let d = Doc {
            title: "Say \"hello\"".into(),
            priority: "P1".into(),
            issue_type: "change".into(),
            epic: "reliability".into(),
            description: "Some context\n\nMore.".into(),
        };
        let text = document(&["bad priority".into()], &d);
        assert!(text.contains("# error: bad priority"));
        assert_eq!(parse(&text).unwrap(), d);
        assert!(parse("no front matter").is_err());
        assert!(parse("+++\nowner = \"x\"\n+++\n").is_err());
        assert!(parse("+++\ntitle = \"Missing type\"\n+++\n").is_err());
    }
}
