//! The call graph (core.load / core._link): every file of a ported language
//! parsed in parallel into the model, modules indexed, calls resolved to
//! edges (`link`).
//!
//! Only languages whose adapter is ported (`lang::ADAPTERS`) are in the
//! graph. A call that Python resolves into a language not ported yet (an
//! Elixir `Foo.bar()` landing on a Kotlin `Foo.kt` through its file stem) has
//! no edge here; `parity --edges` reports any such difference.

pub mod link;
pub mod pylink;

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use indexmap::IndexMap;
use rayon::prelude::*;

use crate::lang::{self, ParseCtx};
use crate::model::{Func, Module};
use crate::project::{ext_of, read_text, Project};

/// (callee or caller id, line, exact)
pub type Edge = (String, usize, bool);

#[derive(Default)]
pub struct Graph {
    /// name -> module, in load order (files in order, modules as parsed)
    pub modules: IndexMap<String, Module>,
    /// file -> its modules' names
    pub by_file: IndexMap<String, Vec<String>>,
    /// function id -> (module name, index in its funcs); a later function
    /// with an id already taken replaces it (as Python's dict does)
    pub funcs: IndexMap<String, (String, usize)>,
    /// function name -> ids, every function of that name
    pub by_name: HashMap<String, Vec<String>>,
    pub edges: IndexMap<String, Vec<Edge>>,
    pub redges: IndexMap<String, Vec<Edge>>,
    /// short name (file stem, declared type) -> modules (path-named languages)
    pub short: HashMap<String, Vec<String>>,
    /// module -> its descendants (set by supertype resolution)
    pub children: HashMap<String, Vec<String>>,
    /// Kotlin / Java package -> its modules
    pub packages: HashMap<String, Vec<String>>,
    /// adapter crashes and the like, as config notes
    pub problems: Vec<String>,
}

/// Godot: "res://path" -> the script's module name (core._res_map).
pub fn res_map(files: &[String]) -> HashMap<String, String> {
    files
        .iter()
        .filter(|r| r.ends_with(".gd"))
        .map(|r| {
            let base = r.rsplit('/').next().unwrap_or(r);
            let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
            (format!("res://{r}"), stem.to_string())
        })
        .collect()
}

/// One file's modules as its adapter gives them (core._parse_one): `Err`
/// when the adapter panicked (Python's "parser error").
pub fn parse_file(p: &Project, res: &HashMap<String, String>, rel: &str) -> Result<Vec<Module>, String> {
    let Some(a) = lang::adapter_for(rel) else { return Ok(Vec::new()) };
    let Some(text) = read_text(&p.path(rel)) else { return Ok(Vec::new()) };
    let ctx = ParseCtx { root: &p.root, res };
    catch_unwind(AssertUnwindSafe(|| a.parse(&ctx, rel, &text))).map_err(|e| {
        e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()
    })
}

/// Every ported file parsed, in file order (core._parse_parallel).
pub fn parse_all(p: &Project, exts: Option<&[String]>) -> Vec<(String, Result<Vec<Module>, String>)> {
    let res = res_map(&p.files);
    let todo: Vec<&String> = p
        .files
        .iter()
        .filter(|r| lang::ported(r) && exts.map(|e| e.iter().any(|x| x == ext_of(r))).unwrap_or(true))
        .collect();
    todo.par_iter().map(|rel| ((*rel).clone(), parse_file(p, &res, rel))).collect()
}

impl Graph {
    /// The linked graph of a project (core.load, less the caches: parsing
    /// here is fast enough to do every run).
    pub fn load(p: &Project) -> Graph {
        let mut g = Graph::assemble(parse_all(p, None));
        link::build_edges(&mut g, p);
        g
    }

    /// Modules indexed by name, file and function (core.load's loop): a
    /// second module of one name becomes `name@file`.
    pub fn assemble(parsed: Vec<(String, Result<Vec<Module>, String>)>) -> Graph {
        let mut g = Graph::default();
        for (rel, mods) in parsed {
            let mods = match mods {
                Ok(m) => m,
                Err(e) => {
                    g.problems.push(format!("{rel}: parser error: {e}"));
                    continue;
                }
            };
            for mut m in mods {
                if g.modules.contains_key(&m.name) {
                    let name = format!("{}@{}", m.name, rel);
                    m.rename(name);
                }
                g.by_file.entry(rel.clone()).or_default().push(m.name.clone());
                for (k, f) in m.funcs.iter().enumerate() {
                    g.funcs.insert(f.id.clone(), (m.name.clone(), k));
                    g.by_name.entry(f.name.clone()).or_default().push(f.id.clone());
                }
                g.modules.insert(m.name.clone(), m);
            }
        }
        g
    }

    pub fn func(&self, id: &str) -> Option<&Func> {
        let (m, k) = self.funcs.get(id)?;
        self.modules.get(m).and_then(|m| m.funcs.get(*k))
    }

    pub fn module(&self, name: &str) -> Option<&Module> {
        self.modules.get(name)
    }

    /// `name` in a module or its parents (core._lookup).
    pub fn lookup(&self, mod_name: &str, name: &str) -> Option<&Func> {
        let mut seen: Vec<&str> = Vec::new();
        let mut cur = mod_name;
        while !cur.is_empty() && !seen.contains(&cur) {
            seen.push(cur);
            let m = self.modules.get(cur)?;
            if let Some(f) = self.func(&format!("{cur}.{name}")) {
                return Some(f);
            }
            cur = &m.parent;
        }
        None
    }

    /// Every edge as `parity --edges` compares it: (caller, callee, line, exact).
    pub fn edge_rows(&self) -> Vec<(String, String, usize, bool)> {
        let mut out = Vec::new();
        for (caller, es) in &self.edges {
            for (t, line, ex) in es {
                out.push((caller.clone(), t.clone(), *line, *ex));
            }
        }
        out
    }
}
