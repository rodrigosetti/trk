//! Render harness profiles with MiniJinja without re-expanding inserted data.

use minijinja::{Environment, ErrorKind, UndefinedBehavior, Value};
use trk_core::error::{Code, Error, Result};

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
        Self { env }
    }

    pub(crate) fn validate(&self, source: &str) -> Result<()> {
        self.env
            .template_from_str(source)
            .map(|_| ())
            .map_err(template_error)
    }

    pub(crate) fn render(&self, source: &str, context: &Value) -> Result<String> {
        self.env
            .template_from_str(source)
            .and_then(|template| template.render(context))
            .map_err(template_error)
    }
}

fn template_error(error: minijinja::Error) -> Error {
    Error::new(Code::ConfigInvalid, format!("harness template: {error:#}"))
}

pub(crate) fn uses_briefing(source: &str) -> bool {
    Environment::new()
        .template_from_str(source)
        .map(|template| template.undeclared_variables(false).contains("briefing"))
        .unwrap_or(false)
}
