//! trk-core: domain types, the four-state issue machine, claims, the waiting
//! queue, the query language, and `Store` (the only writer).

pub mod config;
pub mod dashboard;
pub mod epic;
pub mod error;
pub mod model;
pub mod ops;
pub mod query;
pub mod seed;
pub mod store;
pub mod system;
pub mod view;

pub use error::{Code, Error, Result};
pub use model::State;
pub use store::{Ctx, Store, StoreOpts};

/// The version of the JSON output format.
pub const SCHEMA_VERSION: u32 = 4;
