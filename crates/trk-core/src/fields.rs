//! Field values: parsing `-f` assignments and coercing raw input into the
//! JSON stored for each field kind.

use serde_json::{Value, json};
use winnow::ascii::multispace0;
use winnow::combinator::{alt, opt};
use winnow::prelude::*;
use winnow::token::{rest, take_while};

use crate::error::{Code, Error, Result, invalid_field};
use crate::model::ActorId;
use crate::registry::{FieldDef, FieldKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Set,
    Append,
    Remove,
}

/// One `-f` assignment. `value` is raw input: a JSON string from the CLI, or
/// any JSON value from MCP tools and `--from` files.
#[derive(Debug, Clone, PartialEq)]
pub struct Assign {
    pub field: String,
    pub op: AssignOp,
    pub value: Value,
}

impl Assign {
    pub fn set(field: impl Into<String>, value: impl Into<Value>) -> Self {
        Assign {
            field: field.into(),
            op: AssignOp::Set,
            value: value.into(),
        }
    }
}

fn assign_parser(input: &mut &str) -> winnow::Result<(String, AssignOp, String)> {
    let _ = multispace0.parse_next(input)?;
    let name = take_while(1.., |c: char| {
        c.is_ascii_alphanumeric() || c == '_' || c == '.'
    })
    .parse_next(input)?;
    let op = alt((
        "+=".value(AssignOp::Append),
        "-=".value(AssignOp::Remove),
        "=".value(AssignOp::Set),
    ))
    .parse_next(input)?;
    let value = opt(rest).parse_next(input)?.unwrap_or("");
    Ok((
        name.trim_start_matches("field.").to_string(),
        op,
        value.to_string(),
    ))
}

/// Parse `name=value`, `name+=value` or `name-=value`. Reading `@file`
/// values is the caller's job (see [`Assign`]).
pub fn parse_assign(s: &str) -> Result<(String, AssignOp, String)> {
    let mut input = s;
    assign_parser.parse_next(&mut input).map_err(|_| {
        Error::new(Code::BadArgs, format!("invalid field assignment '{s}'"))
            .with_hint("use -f name=value, -f name+=item, -f name-=item, -f name=@file, -f name=")
    })
}

/// Context needed to coerce values that refer to the outside world.
pub trait CoerceCtx {
    fn me(&self) -> String;
    fn now_rfc3339(&self) -> String;
    /// Normalize and validate a repository reference.
    fn resolve_repo(&self, raw: &str) -> Result<String>;
}

/// True when a stored value counts as set for requirement checks.
pub fn is_present(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(_) => true,
    }
}

pub fn item_count(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Array(a)) => a.len() as i64,
        Some(Value::Null) | None => 0,
        Some(_) => 1,
    }
}

/// Coerce a raw value into the stored JSON for `def`. Returns `Value::Null`
/// for an empty input, meaning "clear".
pub fn coerce(def: &FieldDef, raw: &Value, ctx: &dyn CoerceCtx) -> Result<Value> {
    if raw.is_null() || raw.as_str().is_some_and(|s| s.is_empty()) {
        return Ok(Value::Null);
    }
    match def.kind {
        FieldKind::List => {
            let items: Vec<Value> = match raw {
                Value::Array(a) => a.clone(),
                Value::String(s) if s.trim_start().starts_with('[') => {
                    match serde_json::from_str::<Value>(s) {
                        Ok(Value::Array(a)) => a,
                        _ => return Err(invalid_field(&def.name, "invalid JSON array")),
                    }
                }
                Value::String(s) if def.list_item() == FieldKind::Link => {
                    vec![Value::String(s.clone())]
                }
                Value::String(s) => s
                    .split(',')
                    .map(|x| x.trim())
                    .filter(|x| !x.is_empty())
                    .map(|x| Value::String(x.to_string()))
                    .collect(),
                other => vec![other.clone()],
            };
            let item = item_def(def);
            let mut out = vec![];
            for it in &items {
                let v = coerce_scalar(&item, it, ctx)
                    .map_err(|e| invalid_field(&def.name, e.message()))?;
                if !v.is_null() {
                    out.push(v);
                }
            }
            Ok(Value::Array(out))
        }
        _ => coerce_scalar(def, raw, ctx),
    }
}

/// Coerce one list item (for `+=` and `-=`).
pub fn coerce_item(def: &FieldDef, raw: &Value, ctx: &dyn CoerceCtx) -> Result<Value> {
    if def.kind != FieldKind::List {
        return Err(invalid_field(
            &def.name,
            "only list fields support += and -=",
        ));
    }
    coerce_scalar(&item_def(def), raw, ctx).map_err(|e| invalid_field(&def.name, e.message()))
}

fn item_def(def: &FieldDef) -> FieldDef {
    FieldDef {
        name: def.name.clone(),
        kind: def.list_item(),
        config: def.config.clone(),
        shared: false,
        description: None,
    }
}

fn coerce_scalar(def: &FieldDef, raw: &Value, ctx: &dyn CoerceCtx) -> Result<Value> {
    let name = def.name.as_str();
    if raw.is_null() {
        return Ok(Value::Null);
    }
    let as_str = |raw: &Value| -> Result<String> {
        match raw {
            Value::String(s) => Ok(s.clone()),
            Value::Number(n) => Ok(n.to_string()),
            Value::Bool(b) => Ok(b.to_string()),
            _ => Err(invalid_field(
                name,
                format!("expected a {}, got {raw}", def.kind.as_str()),
            )),
        }
    };
    match def.kind {
        FieldKind::Text | FieldKind::Markdown => {
            let s = as_str(raw)?;
            if s.trim().is_empty() {
                Ok(Value::Null)
            } else {
                Ok(Value::String(s))
            }
        }
        FieldKind::Int => {
            let n = match raw {
                Value::Number(n) => n.as_i64(),
                Value::String(s) => s.trim().parse::<i64>().ok(),
                _ => None,
            }
            .ok_or_else(|| invalid_field(name, format!("expected an integer, got {raw}")))?;
            if let Some(min) = def.config.get("min").and_then(|v| v.as_i64())
                && n < min
            {
                return Err(invalid_field(name, format!("must be at least {min}")));
            }
            if let Some(max) = def.config.get("max").and_then(|v| v.as_i64())
                && n > max
            {
                return Err(invalid_field(name, format!("must be at most {max}")));
            }
            Ok(json!(n))
        }
        FieldKind::Bool => match raw {
            Value::Bool(b) => Ok(json!(b)),
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "y" | "1" | "on" => Ok(json!(true)),
                "false" | "no" | "n" | "0" | "off" => Ok(json!(false)),
                _ => Err(invalid_field(
                    name,
                    format!("expected true or false, got '{s}'"),
                )),
            },
            _ => Err(invalid_field(name, format!("expected a bool, got {raw}"))),
        },
        FieldKind::Date => {
            let s = as_str(raw)?;
            let d: jiff::civil::Date = s.trim().parse().map_err(|_| {
                invalid_field(name, format!("expected a date YYYY-MM-DD, got '{s}'"))
            })?;
            Ok(Value::String(d.to_string()))
        }
        FieldKind::Datetime => {
            let s = as_str(raw)?;
            let s = s.trim();
            if s == "now" {
                return Ok(Value::String(ctx.now_rfc3339()));
            }
            let ts: jiff::Timestamp = s
                .parse()
                .or_else(|_| {
                    s.parse::<jiff::civil::DateTime>().map_err(|_| ()).and_then(
                        |dt: jiff::civil::DateTime| {
                            dt.to_zoned_datetime(jiff::tz::TimeZone::UTC)
                                .map(|z| z.timestamp())
                                .map_err(|_| ())
                        },
                    )
                })
                .map_err(|_| {
                    invalid_field(
                        name,
                        format!("expected an RFC 3339 timestamp or 'now', got '{s}'"),
                    )
                })?;
            Ok(Value::String(crate::model::fmt_ts(ts)))
        }
        FieldKind::Url => {
            let s = as_str(raw)?;
            let s = s.trim();
            let u = url::Url::parse(s)
                .map_err(|e| invalid_field(name, format!("invalid URL '{s}': {e}")))?;
            if u.cannot_be_a_base() && u.scheme() != "mailto" {
                return Err(invalid_field(name, format!("invalid URL '{s}'")));
            }
            Ok(Value::String(s.to_string()))
        }
        FieldKind::Enum => {
            let s = as_str(raw)?;
            let s = s.trim().to_string();
            let values = def.enum_values();
            if !values.contains(&s) {
                return Err(invalid_field(
                    name,
                    format!("'{s}' is not one of: {}", values.join(", ")),
                )
                .with_detail("allowed", json!(values)));
            }
            Ok(Value::String(s))
        }
        FieldKind::Actor => {
            let s = as_str(raw)?;
            let s = s.trim();
            let s = if s == "me" { ctx.me() } else { s.to_string() };
            ActorId::parse(&s).map_err(|e| invalid_field(name, e.message()))?;
            Ok(Value::String(s))
        }
        FieldKind::Repo => {
            let s = as_str(raw)?;
            Ok(Value::String(
                ctx.resolve_repo(s.trim())
                    .map_err(|e| invalid_field(name, e.message()))?,
            ))
        }
        FieldKind::Link => coerce_link(name, raw),
        FieldKind::List => Err(invalid_field(name, "nested lists are not supported")),
    }
}

/// A link is `{kind, url, note}`. From a string: `[kind=]url[ note…]`.
fn coerce_link(name: &str, raw: &Value) -> Result<Value> {
    let (kind, url, note) = match raw {
        Value::Object(o) => (
            o.get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("link")
                .to_string(),
            o.get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            o.get("note").and_then(|v| v.as_str()).map(String::from),
        ),
        Value::String(s) => {
            let s = s.trim();
            if s.starts_with('{') {
                return coerce_link(
                    name,
                    &serde_json::from_str(s).map_err(|e| invalid_field(name, e))?,
                );
            }
            let (head, note) = match s.split_once(char::is_whitespace) {
                Some((h, n)) => (h, Some(n.trim().to_string())),
                None => (s, None),
            };
            match head.split_once('=') {
                Some((k, u)) if !k.contains(':') && !k.contains('/') => {
                    (k.to_string(), u.to_string(), note)
                }
                _ => ("link".to_string(), head.to_string(), note),
            }
        }
        _ => {
            return Err(invalid_field(
                name,
                "expected a link {kind, url, note} or 'kind=url note'",
            ));
        }
    };
    url::Url::parse(&url).map_err(|e| invalid_field(name, format!("invalid URL '{url}': {e}")))?;
    let mut o = serde_json::Map::new();
    o.insert("kind".into(), json!(kind));
    o.insert("url".into(), json!(url));
    if let Some(n) = note.filter(|n| !n.is_empty()) {
        o.insert("note".into(), json!(n));
    }
    Ok(Value::Object(o))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_assignments() {
        assert_eq!(
            parse_assign("severity=high").unwrap(),
            ("severity".into(), AssignOp::Set, "high".into())
        );
        assert_eq!(
            parse_assign("tags+=infra").unwrap(),
            ("tags".into(), AssignOp::Append, "infra".into())
        );
        assert_eq!(
            parse_assign("tags-=infra").unwrap(),
            ("tags".into(), AssignOp::Remove, "infra".into())
        );
        assert_eq!(
            parse_assign("revisit_by=").unwrap(),
            ("revisit_by".into(), AssignOp::Set, "".into())
        );
        assert_eq!(
            parse_assign("links+=pr=https://x/y?a=b").unwrap(),
            (
                "links".into(),
                AssignOp::Append,
                "pr=https://x/y?a=b".into()
            )
        );
        assert_eq!(parse_assign("field.env=prod").unwrap().0, "env");
        assert!(parse_assign("=x").is_err());
        assert!(parse_assign("noop").is_err());
    }
}
