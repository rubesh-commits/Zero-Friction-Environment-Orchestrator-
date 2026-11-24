//! Shared domain model, deterministic pipeline state machine and Linux
//! platform helpers for the Linux orchestrator.

pub mod error;
pub mod fsutil;
pub mod linuxapi;
pub mod model;
pub mod paths;
pub mod process;
pub mod state;
pub mod version;

pub use error::{Error, Result};
pub use model::*;
pub use version::{Version, VersionReq};

pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const PRODUCT_NAME: &str = "LinuxOrchestrator";
pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");