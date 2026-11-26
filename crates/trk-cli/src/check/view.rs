//! Check output and restart handoff share this serialized shape.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use trk_core::model::State;
use trk_core::ops::CheckResult;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CheckView {
    pub issue: String,
    pub title: String,
    #[serde(flatten)]
    pub outcome: CheckOutcome,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum CheckOutcome {
    Passed(CheckExecution),
    Retry(CheckExecution),
    Failed(CheckExecution),
    Woken(CheckExecution),
    Skipped(CheckExecution),
    WouldRun(CheckPreview),
}

impl CheckOutcome {
    pub fn recorded(result: CheckResult, execution: CheckExecution) -> Self {
        match result {
            CheckResult::Passed => Self::Passed(execution),
            CheckResult::Retry => Self::Retry(execution),
            CheckResult::Failed => Self::Failed(execution),
            CheckResult::Woken { .. } => Self::Woken(execution),
            CheckResult::Skipped => Self::Skipped(execution),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CheckExecution {
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub state: State,
    pub next_check_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CheckPreview {
    pub check: Option<String>,
    pub reason: Option<String>,
    pub cwd: Option<PathBuf>,
    pub problem: Option<String>,
}
