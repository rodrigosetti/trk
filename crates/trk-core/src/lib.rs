//! trk-core: domain types, schema registry, config compiler, validation,
//! gate and claim rules, the query language, and `Store` (the only writer).

pub mod config;
pub mod error;
pub mod fields;
pub mod model;
pub mod ops;
pub mod query;
pub mod registry;
pub mod store;
pub mod view;

pub use error::{Code, Error, Result};
pub use registry::Schema;
pub use store::{Ctx, Store, StoreOpts};

/// The version of the JSON output format.
pub const SCHEMA_VERSION: u32 = 1;
