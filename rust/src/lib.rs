//! seamcheck, the Rust port: see the whole codebase at once and check the
//! rules between its parts. Deterministic and local. The CLI, the TOML and
//! the Python test suite are the contract (see PORT.md).

pub mod config;
pub mod elixir;
pub mod gate;
pub mod glob;
pub mod lint;
pub mod output;
pub mod project;
pub mod pyre;
pub mod rules;
pub mod scan;
pub mod walk;

pub const VERSION: &str = "0.3.0";
