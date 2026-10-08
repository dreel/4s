//! Shared types for 4S: the RPC API, events, state snapshots, and the project
//! file format. This crate is the single source of truth; TypeScript bindings
//! and JSON Schema are generated from it (see `src/bin/gen_bindings.rs`).

pub mod api;
pub mod project;
pub mod types;

pub use api::*;
pub use project::*;
pub use types::*;

/// Bumped on any breaking change to the wire protocol.
pub const PROTOCOL_VERSION: u32 = 3;

/// Default address the daemon listens on.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:4440";

/// JSON-RPC notification method used for pushed events.
pub const EVENT_NOTIFICATION: &str = "event";
