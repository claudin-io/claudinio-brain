//! `brain` -- bitemporal knowledge-graph memory for AI agents.
//!
//! One binary, one file, no server, no LLM on the write path.

// In MCP mode stdout is the JSON-RPC transport: a single stray `println!` or
// `dbg!` corrupts the stream and the client disconnects with no diagnostic.
// Diagnostics go to stderr via `tracing`; only the CLI's own output writer is
// allowed to touch stdout.
#![deny(clippy::print_stdout, clippy::dbg_macro)]

pub mod brain;
pub mod cli;
pub mod clock;
pub mod config;
pub mod embed;
pub mod ids;
pub mod index;
pub mod locate;
pub mod norm;
pub mod recall;
pub mod store;
