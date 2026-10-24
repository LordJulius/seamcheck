//! Language adapters: text in, `model::Module`s out (core.adapter_for).
//!
//! Each adapter is a unit struct implementing `Adapter`, registered in
//! `ADAPTERS`. An adapter ported from Python must produce what the Python
//! one does, field for field: `parity --parse --ext <its exts>` proves it.
//! A file whose adapter isn't ported yet has no modules here (`ported`).

use std::collections::HashMap;
use std::path::Path;

use crate::model::Module;
use crate::project::ext_of;

pub mod assign;
pub mod chains;
pub mod elixir;
pub mod gdscript;
pub mod lambdas;
pub mod probe;
pub mod ruby;
pub mod python;

/// What an adapter may consult besides the file's text.
pub struct ParseCtx<'a> {
    pub root: &'a Path,
    /// Godot: "res://path" -> module name (core._res_map)
    pub res: &'a HashMap<String, String>,
}

pub trait Adapter: Sync + Send {
    /// the scan family it blanks with ("elixir", "js", "c"...)
    fn family(&self) -> &'static str;
    /// the extensions it reads, dot included
    fn exts(&self) -> &'static [&'static str];
    /// The file's modules. `rel`: its path from the root; `text`: its text as
    /// Python reads it (`project::read_text`). Never panics on odd input: a
    /// file it can't make sense of gives what it could read.
    fn parse(&self, ctx: &ParseCtx, rel: &str, text: &str) -> Vec<Module>;
}

static ADAPTERS: &[&dyn Adapter] = &[&elixir::Elixir, &python::Python, &gdscript::GdScript, &ruby::Ruby];

/// The ported adapter for a file, if any.
pub fn adapter_for(rel: &str) -> Option<&'static dyn Adapter> {
    let ext = ext_of(rel);
    ADAPTERS.iter().copied().find(|a| a.exts().contains(&ext))
}

/// Is this file's language read in Rust yet? (A file with an adapter in
/// Python but not here has no modules, functions or calls in the Rust graph.)
pub fn ported(rel: &str) -> bool {
    adapter_for(rel).is_some()
}

/// Every extension some ported adapter reads.
pub fn ported_exts() -> Vec<&'static str> {
    ADAPTERS.iter().flat_map(|a| a.exts().iter().copied()).collect()
}
