//! Profile rendering. Translate legacy tokens in template source, never in rendered data.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use minijinja::{Environment, ErrorKind, UndefinedBehavior, Value};
use trk_core::error::{Code, Error, Result};

const SCALARS: &[&str] = &[
    "name",
    "issue",
    "model",
    "actor",
    "agent",
    "harness",
    "cwd",
    "worktree",
    "session_dir",
    "briefing",
    "briefing_file",
    "instructions",
    "instructions_file",
    "mcp_json_file",
    "trk_command",
];

static LEGACY: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
    toml::from_str(include_str!("harness_legacy.toml")).expect("valid legacy profile templates")
});

fn filter_error(error: impl std::error::Error + Send + Sync + 'static) -> minijinja::Error {
    minijinja::Error::new(
        ErrorKind::InvalidOperation,
        "cannot serialize profile value",
    )
    .with_source(error)
}

fn toml_value(value: &toml::Value, out: &mut String) {
    use std::fmt::Write;
    match value {
        toml::Value::Array(values) => {
            out.push('[');
            for (i, value) in values.iter().enumerate() {
                if i != 0 {
                    out.push_str(", ");
                }
                toml_value(value, out);
            }
            out.push(']');
        }
        toml::Value::Table(values) => {
            out.push('{');
            for (i, (key, value)) in values.iter().enumerate() {
                if i != 0 {
                    out.push_str(", ");
                }
                if !key.is_empty()
                    && key
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                {
                    out.push_str(key);
                } else {
                    write!(out, "{}", toml::Value::String(key.clone())).unwrap();
                }
                out.push_str(" = ");
                toml_value(value, out);
            }
            out.push('}');
        }
        _ => write!(out, "{value}").unwrap(),
    }
}

pub(crate) struct Renderer {
    env: Environment<'static>,
}

impl Renderer {
    pub(crate) fn new() -> Self {
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        env.set_syntax(
            minijinja::syntax::SyntaxConfig::builder()
                .keep_trailing_newline(true)
                .build()
                .expect("valid default template syntax"),
        );
        // Harness config is not HTML: keep compact JSON and literal Unicode/HTML characters.
        env.add_filter("tojson", |value: Value| {
            serde_json::to_string(&value).map_err(filter_error)
        });
        env.add_filter("toml", |value: Value| {
            let value = toml::Value::try_from(value).map_err(filter_error)?;
            let mut out = String::new();
            toml_value(&value, &mut out);
            Ok::<_, minijinja::Error>(out)
        });
        for (name, source) in LEGACY.iter() {
            env.add_template(name, source)
                .expect("valid legacy profile template");
        }
        Self { env }
    }

    pub(crate) fn validate(&self, source: &str) -> Result<BTreeSet<String>> {
        let (source, legacy) = translate_legacy(source);
        self.env
            .template_from_str(&source)
            .map_err(template_error)?;
        Ok(legacy)
    }

    pub(crate) fn render(&self, source: &str, context: &Value) -> Result<String> {
        let (source, _) = translate_legacy(source);
        self.env
            .template_from_str(&source)
            .and_then(|template| template.render(context))
            .map_err(template_error)
    }
}

fn template_error(error: minijinja::Error) -> Error {
    Error::new(Code::ConfigInvalid, format!("harness template: {error:#}"))
}

pub(crate) fn uses_briefing(source: &str) -> bool {
    let (source, _) = translate_legacy(source);
    Environment::new()
        .template_from_str(&source)
        .map(|template| template.undeclared_variables(false).contains("briefing"))
        .unwrap_or(false)
}

// Skip whole Jinja tags, respecting quoted delimiters. Their strings/comments/raw
// blocks are source literals, not legacy placeholders.
fn tag_end(source: &str, close: &str, quoted: bool) -> usize {
    let mut quote = None;
    let mut escaped = false;
    let mut depth = 0usize;
    for (i, c) in source.char_indices().skip(2) {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
        } else if depth == 0 && source[i..].starts_with(close) {
            return i + close.len();
        } else if quoted && (c == '\'' || c == '"') {
            quote = Some(c);
        } else if quoted && matches!(c, '{' | '[' | '(') {
            depth += 1;
        } else if quoted && matches!(c, '}' | ']' | ')') {
            depth = depth.saturating_sub(1);
        }
    }
    source.len() // Let MiniJinja report an unterminated tag.
}

fn translate_legacy(source: &str) -> (String, BTreeSet<String>) {
    let mut out = String::with_capacity(source.len());
    let mut legacy = BTreeSet::new();
    let mut rest = source;
    let mut raw = false;
    while let Some(i) = if raw { rest.find("{%") } else { rest.find('{') } {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if raw {
            let end = tag_end(rest, "%}", false);
            let statement = rest
                .get(2..end.saturating_sub(2))
                .unwrap_or("")
                .trim()
                .trim_matches(['-', '+'])
                .trim();
            let end = if rest[..end].ends_with("%}") && statement == "endraw" {
                raw = false;
                end
            } else {
                2
            };
            out.push_str(&rest[..end]);
            rest = &rest[end..];
            continue;
        }
        let tag = if rest.starts_with("{#") {
            Some(("#}", false))
        } else if rest.starts_with("{{") {
            Some(("}}", true))
        } else if rest.starts_with("{%") {
            Some(("%}", true))
        } else {
            None
        };
        if let Some((close, quoted)) = tag {
            let end = tag_end(rest, close, quoted);
            if close == "%}" && rest[..end].ends_with(close) {
                let statement = rest[2..end - 2].trim().trim_matches(['-', '+']).trim();
                if statement == "raw" {
                    raw = true;
                }
            }
            out.push_str(&rest[..end]);
            rest = &rest[end..];
            continue;
        }
        if let Some(end) = rest.find('}') {
            let name = &rest[1..end];
            if SCALARS.contains(&name) {
                out.push_str("{{ ");
                out.push_str(name);
                out.push_str(" }}");
            } else if LEGACY.contains_key(name) {
                out.push_str("{% include ");
                out.push_str(&serde_json::to_string(name).unwrap());
                out.push_str(" %}");
            } else if let Some(prefix) = name.strip_prefix("trk_tools:") {
                out.push_str("{% for tool in trk_tools %}{{ ");
                out.push_str(&serde_json::to_string(prefix).unwrap());
                out.push_str(" }}{{ tool }}{% if not loop.last %},{% endif %}{% endfor %}");
            } else {
                out.push('{');
                rest = &rest[1..];
                continue;
            }
            legacy.insert(name.to_string());
            rest = &rest[end + 1..];
            continue;
        }
        out.push('{');
        rest = &rest[1..];
    }
    out.push_str(rest);
    (out, legacy)
}
