//! Shared domain model, deterministic pipeline state machine and Windows
//! platform helpers for the Windows orchestrator.
//!
//! This crate contains no AI/LLM code. Every decision in the pipeline is made
//! from manifests, parsed syntax and embedded compatibility tables.

pub mod error;
pub mod fsutil;
pub mod model;
pub mod paths;
pub mod process;
pub mod state;
pub mod version;
pub mod winapi;

pub use error::{Error, Result};
pub use model::*;
pub use version::{Version, VersionReq};

/// Version of the restore-file schema emitted by this build.
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Human-facing product name.
pub const PRODUCT_NAME: &str = "WinOrchestrator";

/// Version of the binary, taken from Cargo metadata.
pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");
