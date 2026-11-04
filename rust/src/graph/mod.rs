//! The call graph (core.load / core._link): every file of a ported language
//! parsed in parallel into the model, modules indexed, calls resolved to
//! edges (`link`).
//!
//! Only languages whose adapter is ported (`lang::ADAPTERS`) are in the
//! graph. A call that Python resolves into a language not ported yet (an
//! Elixir `Foo.bar()` landing on a Kotlin `Foo.kt` through its file stem) has
//! no edge here; `parity --edges` reports any such difference.

pub mod bridges;
pub mod cache;
pub mod imports;
pub mod infer;
pub mod link;
pub mod precise;
pub mod pylink;
pub mod qt;
pub mod query;

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use indexmap::IndexMap;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::lang::{self, ParseCtx};
use crate::model::{Func, Module};
use crate::project::{ext_of, read_text, Project};

/// (callee or caller id, line, exact)
pub type Edge = (String, usize, bool);

/// One call site as the linker settled it (core._TRACE): its file, line,
/// name and chain, the outcome ("exact", "guess", "named": nothing though a
/// project function has the name, "nothing") and its first four callees.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceRow {
    pub file: String,
    pub line: usize,
    pub name: String,
    pub chain: Vec<String>,
    pub outcome: &'static str,
    pub ids: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
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
    /// C++ classes registered under a QML name: QML name -> class (core._qml_context)
    pub qml_names: IndexMap<String, String>,
    /// the packs' `[[param]]` / `[[returns]]` entries (config.pack_params)
    #[serde(with = "cache::json_text")]
    pub params: IndexMap<String, serde_json::Value>,
    /// edges a `[[bridge]]` drew: (from, to) -> the bridge's id
    pub bridged: HashMap<(String, String), String>,
    /// notes that a bridge couldn't be built here (warnings)
    pub env_notes: Vec<String>,
    /// per extension, how call sites resolved (core.Project.resolution)
    pub resolution: link::Resolution,
    /// calls by a project function's name that reached none (core.Project.possible)
    pub possible: link::Possible,
    /// how long the load took (core.Project.parse_ms)
    #[serde(skip)]
    pub load_ms: u128,
    /// every call's outcome in resolution order, when asked for (precise mode)
    #[serde(skip)]
    pub trace: Option<Vec<TraceRow>>,
    /// what the language servers settled (precise/), when they were asked
    #[serde(skip)]
    pub precise: Option<precise::Stats>,
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
        Graph::load_with(p, true)
    }

    /// `bridges` off: the call graph alone (parity compares it so). Language
    /// servers settle the guesses when `[precise] enabled` says so.
    pub fn load_with(p: &Project, bridges: bool) -> Graph {
        Graph::load_precise(p, bridges, precise::enabled(p), Some(&precise::cache_dir()))
    }

    /// `precise`: let language servers settle the calls the static resolver
    /// guesses (`--precise`, or `[precise] enabled`); `cache`: the directory
    /// whose answers from earlier runs may answer (None, `--no-cache`: ask
    /// them all).
    pub fn load_precise(p: &Project, bridges: bool, precise: bool, cache: Option<&std::path::Path>) -> Graph {
        Graph::load_kept(p, bridges, precise, cache, true)
    }

    /// The graph of a scratch tree (a commit unpacked to measure it): built every time and not kept.
    pub fn load_scratch(p: &Project, bridges: bool) -> Graph {
        Graph::load_kept(p, bridges, precise::enabled(p), Some(&precise::cache_dir()), false)
    }

    fn load_kept(p: &Project, bridges: bool, precise: bool, cache: Option<&std::path::Path>, keep: bool) -> Graph {
        let t0 = std::time::Instant::now();
        // the linked graph of a tree seen before (not the precise one: language servers settle it)
        let slot = if precise || !keep { None } else { cache::slot(p, bridges) };
        if let Some(mut g) = slot.as_ref().and_then(|s| s.load()) {
            g.load_ms = t0.elapsed().as_millis();
            return g;
        }
        let parsed = parse_all(p, None);
        if std::env::var_os("SEAM_TIMING").is_some() {
            eprintln!("  parse: {} ms", t0.elapsed().as_millis());
        }
        let mut g = Graph::assemble(parsed);
        // what the frameworks in use hand the functions they call (packs' [[param]]):
        // type knowledge, so a detected framework's applies without being listed
        let mut packs: Vec<String> =
            p.config.table("project").get("packs").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default();
        packs.extend(crate::init::detect_packs(&p.root, &p.files));
        packs.sort();
        packs.dedup();
        g.params = crate::config::pack_params(&packs);
        if precise {
            g.trace = Some(Vec::new());
        }
        link::build_edges(&mut g, p);
        if precise {
            precise::refine(&mut g, p, cache);
        }
        if bridges {
            bridges::apply_bridges(&mut g, p);
        }
        g.load_ms = t0.elapsed().as_millis();
        // (a graph with a bridge that couldn't be built here is not the tree's graph: a run with the tools would read it)
        if let Some(s) = slot.as_ref().filter(|_| g.env_notes.is_empty()) {
            s.save(&g, g.load_ms);
        }
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
        let mut key = String::with_capacity(mod_name.len() + name.len() + 8);
        while !cur.is_empty() && !seen.contains(&cur) {
            seen.push(cur);
            let m = self.modules.get(cur)?;
            key.clear();
            key.push_str(cur);
            key.push('.');
            key.push_str(name);
            if let Some(f) = self.func(&key) {
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
