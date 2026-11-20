//! Interactive editing: fields are TOML front matter between `+++` lines and
//! `context` is the markdown body. When validation fails on save, the file
//! reopens with the errors added as comments.

use std::io::Write;
use std::process::Command;

use serde_json::Value;
use trk_core::config::toml_to_json;
use trk_core::error::{Code, Error, Result};
use trk_core::fields::Assign;

pub fn is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn toml_value(v: &Value) -> String {
    match v {
        Value::Null => "\"\"".into(),
        other => {
            let t: Option<toml::Value> = serde_json::from_value(other.clone()).ok();
            t.map(|t| t.to_string())
                .unwrap_or_else(|| format!("{other}"))
        }
    }
}

/// Build the document. `fields` are (name, current value, required, hint).
pub fn document(
    errors: &[String],
    fields: &[(String, Value, bool, String)],
    context: &str,
) -> String {
    let mut s = String::from("+++\n");
    for e in errors {
        s.push_str(&format!("# error: {e}\n"));
    }
    if !errors.is_empty() {
        s.push('\n');
    }
    for (name, value, required, hint) in fields {
        let comment = if hint.is_empty() {
            String::new()
        } else {
            format!("  # {hint}")
        };
        if *required || !value.is_null() {
            s.push_str(&format!("{name} = {}{comment}\n", toml_value(value)));
        } else {
            s.push_str(&format!("# {name} = \"\"{comment}\n"));
        }
    }
    s.push_str("+++\n");
    s.push_str(context);
    if !context.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// Parse the document into assignments; `context` comes from the body.
pub fn parse(doc: &str) -> Result<Vec<Assign>> {
    let rest = doc.trim_start().strip_prefix("+++").ok_or_else(|| {
        Error::new(
            Code::BadArgs,
            "the file must start with a +++ front matter block",
        )
    })?;
    let (front, body) = rest
        .split_once("\n+++")
        .ok_or_else(|| Error::new(Code::BadArgs, "front matter is not closed with +++"))?;
    let body = body.strip_prefix('\n').unwrap_or(body);
    let table: toml::Table = toml::from_str(front)
        .map_err(|e| Error::new(Code::BadArgs, format!("front matter: {e}")))?;
    let mut out = vec![];
    for (k, v) in table {
        let j = toml_to_json(&v);
        if j.as_str().is_some_and(|s| s.is_empty()) {
            continue;
        }
        out.push(Assign::set(k, j));
    }
    let body = body.trim();
    if !body.is_empty() {
        out.push(Assign::set("context", body.to_string()));
    }
    Ok(out)
}

/// Open `$EDITOR` on the document until `apply` accepts it. Returns `apply`'s result.
pub fn edit_loop<T>(
    initial: impl Fn(&[String]) -> String,
    mut apply: impl FnMut(Vec<Assign>) -> Result<T>,
) -> Result<T> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let dir = std::env::temp_dir();
    let path = dir.join(format!("trk-{}.md", std::process::id()));
    let mut errors: Vec<String> = vec![];
    let mut doc = initial(&errors);
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
        let res = parse(&cleaned).and_then(&mut apply);
        match res {
            Ok(v) => {
                let _ = std::fs::remove_file(&path);
                return Ok(v);
            }
            Err(e) => {
                errors = vec![e.message()];
                if let Some(h) = e.hint() {
                    errors.push(format!("hint: {h}"));
                }
                let j = e.to_json();
                if let Some(Value::Array(m)) = j.get("missing_fields")
                    && !m.is_empty()
                {
                    errors.push(format!(
                        "missing: {}",
                        m.iter()
                            .filter_map(|x| x.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                let body_start = cleaned
                    .trim_start()
                    .strip_prefix("+++")
                    .and_then(|r| r.split_once("\n+++"))
                    .map(|(f, b)| (f.to_string(), b.to_string()));
                doc = match body_start {
                    Some((front, body)) => {
                        let mut s = String::from("+++\n");
                        for e in &errors {
                            s.push_str(&format!("# error: {e}\n"));
                        }
                        s.push_str(front.trim_start_matches('\n'));
                        s.push_str("\n+++");
                        s.push_str(&body);
                        s
                    }
                    None => {
                        let mut s = String::new();
                        for e in &errors {
                            s.push_str(&format!("# error: {e}\n"));
                        }
                        s.push_str(&cleaned);
                        s
                    }
                };
                let _ = &initial;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let doc = document(
            &[],
            &[
                (
                    "title".into(),
                    Value::String("Hello".into()),
                    true,
                    String::new(),
                ),
                ("question".into(), Value::Null, true, "required".into()),
                ("doc_url".into(), Value::Null, false, String::new()),
            ],
            "Some context\n",
        );
        let a = parse(&doc).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].field, "title");
        assert_eq!(a[1].field, "context");
    }
}
