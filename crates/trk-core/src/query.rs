//! The query language: one string of terms joined by implicit AND, parsed
//! into a typed AST and compiled to parameterised SQL over `issue_view i`.
//!
//! ```text
//! query  := term (WS term)*
//! term   := NOT? (pred | flag)
//! pred   := name op value | name 'in' '(' value (',' value)* ')'
//! op     := '=' | '!=' | '<' | '<=' | '>' | '>=' | '~'
//! flag   := 'ready' | 'blocked' | 'due' | 'stale-lease'
//! value  := word | quoted | 'me' | '.' | reltime
//! name   := id | state | project | priority | agent | reason | claimed-by | created-by
//!         | title | created | updated | closed | next-check | text | blocks | blocked-by
//! ```

use rusqlite::types::Value as SqlValue;
use winnow::ascii::{multispace0, multispace1};
use winnow::combinator::{alt, delimited, opt, preceded, repeat, separated, terminated};
use winnow::prelude::*;
use winnow::token::{any, none_of, take_while};

use crate::error::{Code, Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Match,
}

impl Op {
    fn sql(self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::Ne => "IS NOT",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Match => "GLOB",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Val {
    Word(String),
    Quoted(String),
}

impl Val {
    pub fn text(&self) -> &str {
        match self {
            Val::Word(s) | Val::Quoted(s) => s,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Ready,
    Blocked,
    Due,
    StaleLease,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    Pred {
        negated: bool,
        name: String,
        op: Op,
        values: Vec<Val>,
        is_in: bool,
    },
    Flag {
        negated: bool,
        flag: Flag,
    },
}

// ----------------------------------------------------------------------
// Parser
// ----------------------------------------------------------------------

fn quoted(input: &mut &str) -> winnow::Result<String> {
    delimited(
        '"',
        repeat(0.., alt((preceded('\\', any), none_of(['"', '\\'])))).fold(
            String::new,
            |mut s, c| {
                s.push(c);
                s
            },
        ),
        '"',
    )
    .parse_next(input)
}

fn word(input: &mut &str) -> winnow::Result<String> {
    take_while(1.., |c: char| {
        !c.is_whitespace() && c != ',' && c != ')' && c != '(' && c != '"'
    })
    .map(String::from)
    .parse_next(input)
}

fn value(input: &mut &str) -> winnow::Result<Val> {
    alt((quoted.map(Val::Quoted), word.map(Val::Word))).parse_next(input)
}

fn name(input: &mut &str) -> winnow::Result<String> {
    (
        take_while(1, |c: char| c.is_ascii_alphabetic()),
        take_while(0.., |c: char| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')
        }),
    )
        .take()
        .map(String::from)
        .parse_next(input)
}

fn op(input: &mut &str) -> winnow::Result<Op> {
    alt((
        "!=".value(Op::Ne),
        "<=".value(Op::Le),
        ">=".value(Op::Ge),
        "=".value(Op::Eq),
        "<".value(Op::Lt),
        ">".value(Op::Gt),
        "~".value(Op::Match),
    ))
    .parse_next(input)
}

fn in_list(input: &mut &str) -> winnow::Result<Vec<Val>> {
    preceded(
        (multispace1, "in", multispace0, '('),
        terminated(
            separated(1.., delimited(multispace0, value, multispace0), ','),
            ')',
        ),
    )
    .parse_next(input)
}

fn negation(input: &mut &str) -> winnow::Result<bool> {
    opt(alt((
        terminated(alt(("NOT", "not")), multispace1).void(),
        "!".void(),
        "-".void(),
    )))
    .map(|o| o.is_some())
    .parse_next(input)
}

/// (negated, name, Some((op, values, is_in)) for predicates | None for flags)
type RawTerm = (bool, String, Option<(Op, Vec<Val>, bool)>);

fn term(input: &mut &str) -> winnow::Result<RawTerm> {
    let negated = negation.parse_next(input)?;
    let n = name.parse_next(input)?;
    let rest = opt(alt((
        in_list.map(|v| (Op::Eq, v, true)),
        (op, value).map(|(o, v)| (o, vec![v], false)),
    )))
    .parse_next(input)?;
    Ok((negated, n, rest))
}

pub fn parse(q: &str) -> Result<Vec<Term>> {
    let mut input = q.trim();
    let mut terms = vec![];
    while !input.is_empty() {
        let before = input;
        let (negated, n, rest) = term.parse_next(&mut input).map_err(|_| {
            Error::new(Code::BadQuery, format!("cannot parse query at '{before}'"))
                .with_hint("terms look like name=value, name in (a,b), text~\"words\", or a flag: ready blocked due stale-lease")
        })?;
        let t = match rest {
            Some((op, values, is_in)) => Term::Pred {
                negated,
                name: n,
                op,
                values,
                is_in,
            },
            None => {
                let flag = match n.as_str() {
                    "ready" => Flag::Ready,
                    "blocked" => Flag::Blocked,
                    "due" => Flag::Due,
                    "stale-lease" => Flag::StaleLease,
                    _ => {
                        return Err(Error::new(Code::BadQuery, format!("unknown flag '{n}'")).with_hint(
                            "flags are ready, blocked, due, stale-lease; predicates need an operator, e.g. state=open",
                        ));
                    }
                };
                Term::Flag { negated, flag }
            }
        };
        terms.push(t);
        let ws: winnow::Result<&str> = multispace0.parse_next(&mut input);
        let consumed_ws = ws.map(|w| !w.is_empty()).unwrap_or(false);
        if !input.is_empty() && !consumed_ws {
            return Err(
                Error::new(Code::BadQuery, format!("unexpected input at '{input}'"))
                    .with_hint("separate terms with spaces; quote values containing spaces"),
            );
        }
    }
    Ok(terms)
}

// ----------------------------------------------------------------------
// Compiler
// ----------------------------------------------------------------------

pub trait QueryCtx {
    fn me(&self) -> Option<String>;
    fn current_project(&self) -> Option<String>;
    fn now(&self) -> jiff::Timestamp;
    fn resolve_id(&self, s: &str) -> Result<String>;
}

pub struct Compiled {
    pub where_sql: String,
    pub params: Vec<SqlValue>,
}

enum Target {
    Column(&'static str),
    Time(&'static str),
    Text,
    /// Issues that block the given one.
    Blocks,
    /// Issues blocked by the given one.
    BlockedBy,
}

const NAMES: &str = "id state project priority agent reason claimed-by created-by title created updated closed next-check text blocks blocked-by";

fn target(name: &str) -> Result<Target> {
    Ok(match name {
        "id" => Target::Column("i.id"),
        "state" | "status" => Target::Column("i.state"),
        "project" => Target::Column("i.project_id"),
        "priority" => Target::Column("i.priority"),
        "agent" => Target::Column("i.agent"),
        "reason" => Target::Column("i.close_reason"),
        "claimed-by" => Target::Column("i.claimant"),
        "created-by" => Target::Column("i.created_by"),
        "title" => Target::Column("i.title"),
        "created" => Target::Time("i.created_at"),
        "updated" => Target::Time("i.updated_at"),
        "closed" => Target::Time("i.closed_at"),
        "next-check" => Target::Time("i.next_check_at"),
        "text" => Target::Text,
        "blocks" => Target::Blocks,
        "blocked-by" => Target::BlockedBy,
        n => {
            return Err(Error::new(Code::BadQuery, format!("unknown name '{n}'"))
                .with_hint(format!("names: {NAMES}")));
        }
    })
}

fn is_glob(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}

/// Resolve `me`, `.` and relative times.
fn resolve_value(name: &str, v: &Val, ctx: &dyn QueryCtx, time: bool) -> Result<SqlValue> {
    let raw = v.text();
    if let Val::Word(w) = v {
        if w == "me" {
            return ctx.me().map(SqlValue::Text).ok_or_else(|| {
                Error::new(Code::BadQuery, "'me' used but no acting actor is known")
                    .with_hint("set user.email in config.toml, TRK_ACTOR, or --as")
            });
        }
        if w == "." && name == "project" {
            return ctx.current_project().map(SqlValue::Text).ok_or_else(|| {
                Error::new(
                    Code::UnknownRepo,
                    "'.' used but the current directory is not a registered repo",
                )
                .with_hint("run `trk repo add` in the repository, or pass --project")
            });
        }
    }
    if time {
        return Ok(SqlValue::Text(resolve_time(raw, ctx.now())?));
    }
    if let Ok(n) = raw.parse::<i64>()
        && matches!(v, Val::Word(_))
    {
        return Ok(SqlValue::Integer(n));
    }
    Ok(SqlValue::Text(raw.to_string()))
}

/// `-7d`, `-12h`, `-30m` relative to now; dates and RFC 3339 timestamps as is.
pub fn resolve_time(raw: &str, now: jiff::Timestamp) -> Result<String> {
    if let Some(rel) = raw.strip_prefix('-').or_else(|| raw.strip_prefix('+')) {
        let d = crate::config::parse_duration(rel)
            .map_err(|_| Error::new(Code::BadQuery, format!("invalid relative time '{raw}'")))?;
        let ts = if raw.starts_with('-') {
            now - d
        } else {
            now + d
        };
        return Ok(crate::model::fmt_ts(ts));
    }
    if raw == "now" {
        return Ok(crate::model::fmt_ts(now));
    }
    if let Ok(d) = raw.parse::<jiff::civil::Date>() {
        return Ok(format!("{d}T00:00:00Z"));
    }
    if let Ok(ts) = raw.parse::<jiff::Timestamp>() {
        return Ok(crate::model::fmt_ts(ts));
    }
    Err(Error::new(Code::BadQuery, format!("invalid time '{raw}'"))
        .with_hint("use -7d, -12h, 2026-11-01, or an RFC 3339 timestamp"))
}

/// SQL fragment comparing `expr` to one value with `op`.
fn cmp(expr: &str, op: Op, v: SqlValue, params: &mut Vec<SqlValue>) -> String {
    match (&op, &v) {
        (Op::Match, SqlValue::Text(s)) if !is_glob(s) => {
            params.push(SqlValue::Text(format!("%{s}%")));
            format!("{expr} LIKE ?")
        }
        (Op::Eq, SqlValue::Text(s)) if is_glob(s) => {
            params.push(v);
            format!("{expr} GLOB ?")
        }
        (Op::Ne, SqlValue::Text(s)) if is_glob(s) => {
            params.push(v);
            format!("({expr} IS NULL OR {expr} NOT GLOB ?)")
        }
        _ => {
            params.push(v);
            format!("{expr} {} ?", op.sql())
        }
    }
}

pub fn compile(terms: &[Term], ctx: &dyn QueryCtx) -> Result<Compiled> {
    let mut parts = vec![];
    let mut params: Vec<SqlValue> = vec![];
    for t in terms {
        let (negated, sql) = match t {
            Term::Flag { negated, flag } => {
                let sql = match flag {
                    Flag::Ready => "i.id IN (SELECT id FROM ready)".to_string(),
                    Flag::Blocked => "i.state <> 'closed' AND EXISTS (SELECT 1 FROM dep d JOIN issue b ON b.id = d.src WHERE d.dst = i.id AND b.state <> 'closed')".to_string(),
                    Flag::Due => {
                        params.push(SqlValue::Text(crate::model::fmt_ts(ctx.now())));
                        "(i.state = 'waiting' AND i.waiting_check IS NOT NULL AND (i.next_check_at IS NULL OR i.next_check_at <= ?))".to_string()
                    }
                    Flag::StaleLease => {
                        params.push(SqlValue::Text(crate::model::fmt_ts(ctx.now())));
                        "(i.lease_until IS NOT NULL AND i.lease_until < ?)".to_string()
                    }
                };
                (*negated, sql)
            }
            Term::Pred {
                negated,
                name,
                op,
                values,
                is_in,
            } => {
                let tgt = target(name)?;
                let time = matches!(tgt, Target::Time(_));
                let one = |v: &Val, params: &mut Vec<SqlValue>| -> Result<String> {
                    Ok(match &tgt {
                        Target::Column(c) => {
                            let sv = resolve_value(name, v, ctx, false)?;
                            let sv = match sv {
                                SqlValue::Integer(n) => SqlValue::Text(n.to_string()),
                                x => x,
                            };
                            let sv = if *c == "i.priority" {
                                match sv {
                                    SqlValue::Text(s) => SqlValue::Text(s.to_ascii_uppercase()),
                                    x => x,
                                }
                            } else {
                                sv
                            };
                            let sv = if *c == "i.id" {
                                match &sv {
                                    SqlValue::Text(s) if !is_glob(s) && *op == Op::Eq => {
                                        SqlValue::Text(ctx.resolve_id(s)?)
                                    }
                                    _ => sv,
                                }
                            } else {
                                sv
                            };
                            cmp(c, *op, sv, params)
                        }
                        Target::Time(c) => cmp(c, *op, resolve_value(name, v, ctx, time)?, params),
                        Target::Text => {
                            let q = match v {
                                Val::Quoted(s) => format!("\"{}\"", s.replace('"', "\"\"")),
                                Val::Word(s) => s.clone(),
                            };
                            params.push(SqlValue::Text(q));
                            let e = "i.id IN (SELECT issue_id FROM search WHERE search MATCH ?)"
                                .to_string();
                            if *op == Op::Ne { format!("NOT {e}") } else { e }
                        }
                        Target::Blocks | Target::BlockedBy => {
                            let other = ctx.resolve_id(v.text())?;
                            params.push(SqlValue::Text(other));
                            let e = if matches!(tgt, Target::Blocks) {
                                "EXISTS (SELECT 1 FROM dep d WHERE d.src = i.id AND d.dst = ?)"
                            } else {
                                "EXISTS (SELECT 1 FROM dep d WHERE d.dst = i.id AND d.src = ?)"
                            };
                            if *op == Op::Ne {
                                format!("NOT {e}")
                            } else {
                                e.to_string()
                            }
                        }
                    })
                };
                let sql = if *is_in {
                    let mut ors = vec![];
                    for v in values {
                        ors.push(one(v, &mut params)?);
                    }
                    format!("({})", ors.join(" OR "))
                } else {
                    one(&values[0], &mut params)?
                };
                (*negated, sql)
            }
        };
        parts.push(if negated {
            format!("NOT coalesce(({sql}), 0)")
        } else {
            format!("({sql})")
        });
    }
    Ok(Compiled {
        where_sql: if parts.is_empty() {
            "1".into()
        } else {
            parts.join(" AND ")
        },
        params,
    })
}

/// ORDER BY clause for `--sort`. A leading `-` sorts descending. Without
/// one, issues are ordered by priority then age.
pub fn order_by(sort: Option<&str>) -> Result<String> {
    let Some(sort) = sort else {
        return Ok("i.priority, i.created_at, i.id".into());
    };
    let mut out = vec![];
    for part in sort.split(',') {
        let (desc, name) = match part.strip_prefix('-') {
            Some(n) => (true, n),
            None => (false, part),
        };
        let col = match name {
            "id" => "i.id",
            "state" | "status" => "i.state",
            "project" => "i.project_id",
            "title" => "i.title",
            "priority" => "i.priority",
            "agent" => "i.agent",
            "created" => "i.created_at",
            "updated" => "i.updated_at",
            "closed" => "i.closed_at",
            "claimed-by" => "i.claimant",
            "lease" => "i.lease_until",
            "next-check" => "i.next_check_at",
            f => {
                return Err(Error::new(
                    Code::BadArgs,
                    format!("cannot sort by '{f}'"),
                )
                .with_hint("sort by: id state project title priority agent created updated closed claimed-by lease next-check"));
            }
        };
        out.push(format!("{col} {}", if desc { "DESC" } else { "ASC" }));
    }
    out.push("i.id".into());
    Ok(out.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_examples() {
        let t = parse("project=orch priority=P1 state!=closed").unwrap();
        assert_eq!(t.len(), 3);
        let t = parse("state in (open,waiting) closed>-7d reason=done").unwrap();
        assert!(matches!(&t[0], Term::Pred { is_in: true, values, .. } if values.len() == 2));
        assert!(
            matches!(&t[1], Term::Pred { op: Op::Gt, values, .. } if values[0] == Val::Word("-7d".into()))
        );
        let t = parse("claimed-by=agent:claude-code:*").unwrap();
        assert!(
            matches!(&t[0], Term::Pred { name, values, .. } if name == "claimed-by" && values[0].text() == "agent:claude-code:*")
        );
        let t = parse("ready project=.").unwrap();
        assert!(matches!(
            t[0],
            Term::Flag {
                flag: Flag::Ready,
                negated: false
            }
        ));
        let t = parse("text~\"feature flag\"").unwrap();
        assert!(
            matches!(&t[0], Term::Pred { op: Op::Match, values, .. } if values[0] == Val::Quoted("feature flag".into()))
        );
        let t = parse("NOT blocked !due blocks=orch-1").unwrap();
        assert!(matches!(
            t[0],
            Term::Flag {
                flag: Flag::Blocked,
                negated: true
            }
        ));
        assert!(matches!(
            t[1],
            Term::Flag {
                flag: Flag::Due,
                negated: true
            }
        ));
        assert!(matches!(&t[2], Term::Pred { name, .. } if name == "blocks"));
        assert!(target("type").is_err());
        assert!(parse("bogus").is_err());
        assert!(parse("state=a\"b").is_err());
        assert!(parse("").unwrap().is_empty());
    }

    #[test]
    fn times() {
        let now: jiff::Timestamp = "2026-11-20T12:00:00Z".parse().unwrap();
        assert_eq!(resolve_time("-7d", now).unwrap(), "2026-11-13T12:00:00Z");
        assert_eq!(resolve_time("-12h", now).unwrap(), "2026-11-20T00:00:00Z");
        assert_eq!(
            resolve_time("2026-11-01", now).unwrap(),
            "2026-11-01T00:00:00Z"
        );
        assert!(resolve_time("yesterday", now).is_err());
    }
}
