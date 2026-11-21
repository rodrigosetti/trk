//! Typed errors. Every variant maps to one JSON error `code` and one exit code.

use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}", .0.message)]
    Coded(Box<Coded>),
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration error: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// A domain error with a stable code and optional structured detail.
#[derive(Debug, Clone, Serialize)]
pub struct Coded {
    pub code: Code,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Extra keys merged into the JSON error object (claimant, blocked_by, …).
    #[serde(flatten)]
    pub detail: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    Internal,
    DbCorrupt,
    BadArgs,
    BadQuery,
    NotFound,
    AmbiguousId,
    UnknownRepo,
    InvalidField,
    InvalidState,
    Blocked,
    Cycle,
    ConfigInvalid,
    OldDatabase,
    ClaimedByOther,
    NotClaimant,
    Busy,
    NotPermitted,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::Internal => "internal",
            Code::DbCorrupt => "db_corrupt",
            Code::BadArgs => "bad_args",
            Code::BadQuery => "bad_query",
            Code::NotFound => "not_found",
            Code::AmbiguousId => "ambiguous_id",
            Code::UnknownRepo => "unknown_repo",
            Code::InvalidField => "invalid_field",
            Code::InvalidState => "invalid_state",
            Code::Blocked => "blocked",
            Code::Cycle => "cycle",
            Code::ConfigInvalid => "config_invalid",
            Code::OldDatabase => "old_database",
            Code::ClaimedByOther => "claimed_by_other",
            Code::NotClaimant => "not_claimant",
            Code::Busy => "busy",
            Code::NotPermitted => "not_permitted",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            Code::Internal | Code::DbCorrupt | Code::OldDatabase => 1,
            Code::BadArgs | Code::BadQuery => 2,
            Code::NotFound | Code::AmbiguousId | Code::UnknownRepo => 3,
            Code::InvalidField | Code::InvalidState | Code::Cycle | Code::ConfigInvalid => 4,
            Code::ClaimedByOther
            | Code::NotClaimant
            | Code::Blocked
            | Code::Busy
            | Code::NotPermitted => 5,
        }
    }
}

impl Error {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Error::Coded(Box::new(Coded {
            code,
            message: message.into(),
            issue: None,
            hint: None,
            detail: Map::new(),
        }))
    }

    pub fn with_issue(mut self, id: impl Into<String>) -> Self {
        if let Error::Coded(c) = &mut self {
            c.issue = Some(id.into());
        }
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        if let Error::Coded(c) = &mut self {
            c.hint = Some(hint.into());
        }
        self
    }

    pub fn with_detail(mut self, key: &str, value: impl Into<Value>) -> Self {
        if let Error::Coded(c) = &mut self {
            c.detail.insert(key.to_string(), value.into());
        }
        self
    }

    pub fn code(&self) -> Code {
        match self {
            Error::Coded(c) => c.code,
            Error::Sqlite(e) => match e.sqlite_error_code() {
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
                    Code::Busy
                }
                Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase) => {
                    Code::DbCorrupt
                }
                _ => Code::Internal,
            },
            _ => Code::Internal,
        }
    }

    pub fn exit_code(&self) -> i32 {
        self.code().exit_code()
    }

    /// The JSON `error` object of the envelope.
    pub fn to_json(&self) -> Value {
        match self {
            Error::Coded(c) => {
                let mut v = serde_json::to_value(c.as_ref()).unwrap_or(Value::Null);
                if let Value::Object(m) = &mut v {
                    m.insert("code".into(), Value::String(c.code.as_str().into()));
                }
                v
            }
            other => serde_json::json!({
                "code": other.code().as_str(),
                "message": other.to_string(),
            }),
        }
    }

    pub fn message(&self) -> String {
        self.to_string()
    }

    pub fn hint(&self) -> Option<&str> {
        match self {
            Error::Coded(c) => c.hint.as_deref(),
            _ => None,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub fn bad_args(msg: impl Into<String>) -> Error {
    Error::new(Code::BadArgs, msg)
}

pub fn not_found(what: &str, id: &str) -> Error {
    Error::new(Code::NotFound, format!("{what} '{id}' not found"))
}

pub fn invalid_field(field: &str, msg: impl std::fmt::Display) -> Error {
    Error::new(Code::InvalidField, format!("field '{field}': {msg}"))
        .with_detail("field", field.to_string())
}
