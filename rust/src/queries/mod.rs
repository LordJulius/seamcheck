//! The commands' answers as data (queries.py). The CLI prints them;
//! `seamcheck serve` returns them as JSON. One implementation, so an agent
//! asking over MCP gets the answer the terminal gets.
//!
//! Every answer is a `serde_json::Value` with Python's keys in Python's
//! order, so `--json` and the server print what the Python ones print.

pub mod cli;

use std::collections::{BTreeSet, HashMap};

use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use crate::gate::{changed_lines_with, is_test};
use crate::glob::match_any;
use crate::graph::link::family;
use crate::graph::Graph;
use crate::model::Func;
use crate::project::{ext_of, Project};
use crate::pyre::{strip, take_chars, Rx};

/// A loaded project and its graph, with the bridges each function sits on.
pub struct Q<'a> {
    pub p: &'a Project,
    pub g: &'a Graph,
    via: HashMap<String, BTreeSet<String>>,
}

fn shares(a: &str, b: &str) -> bool {
    let fb = family(b);
    family(a).iter().any(|x| fb.contains(x))
}

impl<'a> Q<'a> {
    pub fn new(p: &'a Project, g: &'a Graph) -> Q<'a> {
        let mut via: HashMap<String, BTreeSet<String>> = HashMap::new();
        for ((a, d), b) in &g.bridged {
            via.entry(a.clone()).or_default().insert(b.clone());
            via.entry(d.clone()).or_default().insert(b.clone());
        }
        Q { p, g, via }
    }

    /// A function's id, place and the bridges it sits on (queries.where).
    pub fn where_(&self, fid: &str) -> Map<String, Value> {
        let f = self.g.func(fid);
        let via: Vec<&String> = self.via.get(fid).map(|v| v.iter().collect()).unwrap_or_default();
        let mut m = Map::new();
        m.insert("id".into(), json!(fid));
        m.insert("file".into(), json!(f.map(|f| f.file.as_str()).unwrap_or("")));
        m.insert("line".into(), json!(f.map(|f| f.start).unwrap_or(0)));
        m.insert("via".into(), json!(via));
        m
    }

    fn where_with(&self, fid: &str, k: &str, v: Value) -> Value {
        let mut m = self.where_(fid);
        m.insert(k.into(), v);
        Value::Object(m)
    }

    /// `callers` (reverse) / `callees`: per matching function, its exact and
    /// name-guess edge counts and what it reaches within `depth` calls (exact
    /// edges unless `fuzzy`). `wide` (callers): also the possible callers,
    /// listed apart (queries.walk).
    pub fn walk(&self, spec: &str, depth: usize, reverse: bool, fuzzy: bool, wide: bool) -> Vec<Value> {
        let mut out = Vec::new();
        for s in self.g.find(spec) {
            let (ex, guess) = self.g.edge_split(&s.id, reverse);
            let dist = self.g.reach(&s.id, depth, reverse, !fuzzy && !wide);
            let mut far: Vec<(&String, &usize)> = dist.iter().filter(|(_, d)| **d > 0).collect();
            far.sort_by(|a, b| (a.1, a.0).cmp(&(b.1, b.0)));
            let mut row = self.where_(&s.id);
            row.insert("exact".into(), json!(ex));
            row.insert("guess".into(), json!(guess));
            row.insert("reach".into(), Value::Array(far.iter().map(|(k, d)| self.where_with(k, "depth", json!(d))).collect()));
            if reverse {
                let poss = self.possible_callers(s);
                row.insert("possible".into(), json!(poss.len()));
                if wide {
                    let sites = poss.iter().map(|(fid, ln)| self.where_with(fid, "line", json!(ln))).collect();
                    row.insert("possible_sites".into(), Value::Array(sites));
                }
            }
            out.push(Value::Object(row));
        }
        out
    }

    /// Call sites named like g that reached no project function: a receiver
    /// typed as a library's may still be one of the project's own. Not
    /// edges: a third tier below exact and name guess (queries.possible_callers).
    pub fn possible_callers(&self, g: &Func) -> Vec<(String, usize)> {
        let arity: Option<Vec<&str>> =
            (!g.targs.is_empty() && g.file.ends_with(".cs")).then(|| g.targs.split(',').collect());
        let mut got: BTreeSet<(String, usize)> = BTreeSet::new();
        for (fid, ln, targs) in self.g.possible.get(&g.name).map(|v| v.as_slice()).unwrap_or(&[]) {
            let Some(caller) = self.g.func(fid) else { continue };
            if !shares(&g.file, &caller.file) {
                continue;
            }
            // (`Read<int>()`: not `Read()`)
            if let Some(a) = &arity {
                if *targs > 0 && !a.contains(&targs.to_string().as_str()) {
                    continue;
                }
            }
            got.insert((fid.clone(), *ln));
        }
        got.into_iter().collect()
    }

    /// The shortest call chain from `a` to `b`, and (when there is no exact
    /// one) whether a chain through name guesses exists (queries.shortest).
    pub fn shortest(&self, a: &str, b: &str, fuzzy: bool) -> (Vec<String>, bool) {
        let froms = self.g.find(a);
        let mut tos: Vec<&str> = Vec::new();
        for f in self.g.find(b) {
            if !tos.contains(&f.id.as_str()) {
                tos.push(&f.id);
            }
        }
        let best = |exact_only: bool| -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            for f in &froms {
                for t in &tos {
                    let c = self.g.path(&f.id, t, 12, exact_only);
                    if !c.is_empty() && (out.is_empty() || c.len() < out.len()) {
                        out = c;
                    }
                }
            }
            out
        };
        let chain = best(!fuzzy);
        let through = chain.is_empty() && !fuzzy && !best(false).is_empty();
        (chain, through)
    }

    /// Where functions are defined (queries.find).
    pub fn find(&self, spec: &str) -> Vec<Value> {
        self.g
            .find(spec)
            .into_iter()
            .map(|f| {
                let (ex, guess) = self.g.edge_split(&f.id, true);
                json!({"id": f.id, "file": f.file, "start": f.start, "end": f.end, "calls": f.calls.len(),
                       "exact_callers": ex, "guess_callers": guess})
            })
            .collect()
    }

    /// Lines matching a regex (`code`: comments and string contents left
    /// out), each with the function it is in and that function's callers
    /// (queries.search). Err: the pattern isn't a regex.
    pub fn search(&self, pattern: &str, globs: &[String], code: bool, limit: usize) -> Result<Vec<Value>, String> {
        let rx = Rx::new(pattern)?;
        let mut out = Vec::new();
        let mut files: Vec<&String> = self.g.by_file.keys().collect();
        files.sort();
        for rel in files {
            if !globs.is_empty() && !match_any(rel, globs) {
                continue;
            }
            let mods: Vec<&crate::model::Module> = self.g.by_file[rel].iter().filter_map(|n| self.g.modules.get(n)).collect();
            let Some(m) = mods.first() else { continue };
            let lines = if code { &m.code } else { &m.raw };
            for (k, ln) in lines.iter().enumerate() {
                let i = k + 1;
                if !rx.is_match(ln) {
                    continue;
                }
                let f = mods.iter().find_map(|md| md.func_at(i).filter(|g| g.kind != "top"));
                let text = m.raw.get(i - 1).map(|s| take_chars(strip(s), 200)).unwrap_or("");
                let mut row = Map::new();
                row.insert("file".into(), json!(rel));
                row.insert("line".into(), json!(i));
                row.insert("text".into(), json!(text));
                row.insert("func".into(), f.map(|f| json!(f.id)).unwrap_or(Value::Null));
                if let Some(f) = f {
                    let (ex, guess) = self.g.edge_split(&f.id, true);
                    row.insert("exact_callers".into(), json!(ex));
                    row.insert("guess_callers".into(), json!(guess));
                }
                out.push(Value::Object(row));
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }

    /// The functions a diff touched (committed since REF, staged, unstaged,
    /// untracked) (queries.changed_functions).
    pub fn changed_functions(&self, since: &str, staged: bool) -> BTreeSet<String> {
        let mut start = BTreeSet::new();
        for (rel, lines) in changed_lines_with(self.p, since, staged, true) {
            for n in self.g.by_file.get(&rel).map(|v| v.as_slice()).unwrap_or(&[]) {
                let m = &self.g.modules[n];
                for ln in &lines {
                    if let Some(f) = m.func_at(*ln).filter(|f| f.kind != "top") {
                        start.insert(f.id.clone());
                    }
                }
            }
        }
        start
    }

    /// Each target a file (all its functions) or a function spec (queries.target_functions).
    pub fn target_functions(&self, targets: &[String]) -> BTreeSet<String> {
        let mut start = BTreeSet::new();
        for t in targets {
            if self.g.by_file.contains_key(t) || self.p.root.join(t).exists() {
                for n in self.g.by_file.get(t).map(|v| v.as_slice()).unwrap_or(&[]) {
                    start.extend(self.g.modules[n].funcs.iter().map(|f| f.id.clone()));
                }
            } else {
                start.extend(self.g.find(t).into_iter().map(|f| f.id.clone()));
            }
        }
        start
    }

    fn area(&self, rel: &str) -> String {
        if let Some(areas) = self.p.config.table("project").get("areas").and_then(|a| a.as_object()) {
            for (k, globs) in areas {
                let gs: Vec<String> = match globs {
                    Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                    Value::String(s) => s.chars().map(String::from).collect(), // (Python iterates a string's characters)
                    _ => vec![],
                };
                if match_any(rel, &gs) {
                    return k.clone();
                }
            }
        }
        rel.split('/').next().unwrap_or("").to_string()
    }

    /// What a change to `start` reaches, by area ([project] areas, else the
    /// top folder): {area: [(distance, function id)]}, and the start's own
    /// areas (queries.impact).
    pub fn impact(&self, start: &BTreeSet<String>, depth: usize, fuzzy: bool) -> (IndexMap<String, Vec<(usize, String)>>, BTreeSet<String>) {
        let start_v: Vec<String> = start.iter().cloned().collect();
        let dist = self.g.impact(&start_v, depth, fuzzy);
        let mut by: IndexMap<String, Vec<(usize, String)>> = IndexMap::new();
        for (fid, d) in &dist {
            if *d > 0 {
                if let Some(f) = self.g.func(fid) {
                    by.entry(self.area(&f.file)).or_default().push((*d, fid.clone()));
                }
            }
        }
        for v in by.values_mut() {
            v.sort();
        }
        let own = start.iter().filter_map(|s| self.g.func(s)).map(|f| self.area(&f.file)).collect();
        (by, own)
    }

    /// `impact --json`: {area: [[distance, id]]}.
    pub fn impact_json(by: &IndexMap<String, Vec<(usize, String)>>) -> Value {
        Value::Object(by.iter().map(|(k, v)| (k.clone(), json!(v.iter().map(|(d, f)| json!([d, f])).collect::<Vec<_>>()))).collect())
    }

    /// What an agent wants before editing a function, in one answer: where
    /// it is, its source, who calls it, what it calls and which tests reach
    /// it (queries.context).
    pub fn context(&self, spec: &str, lines: usize, depth: usize) -> Vec<Value> {
        let g = self.g;
        let mut out = Vec::new();
        for f in g.find(spec) {
            let md = g.by_file.get(&f.file).and_then(|ns| ns.iter().map(|n| &g.modules[n]).find(|m| m.name == f.module));
            let src: &[String] = match md {
                Some(m) => {
                    let a = f.start.saturating_sub(1).min(m.raw.len());
                    &m.raw[a..f.end.min(m.raw.len()).max(a)]
                }
                None => &[],
            };
            let mut callers: Vec<(bool, &str, usize, &str)> = g
                .redges
                .get(&f.id)
                .map(|v| v.as_slice())
                .unwrap_or(&[])
                .iter()
                .filter_map(|(s, ln, e)| g.func(s).map(|sf| (*e, sf.file.as_str(), *ln, s.as_str())))
                .collect();
            callers.sort_by(|a, b| (!a.0, a.1, a.2).cmp(&(!b.0, b.1, b.2)));
            let mut by_line: HashMap<(usize, &str), Vec<(&str, bool)>> = HashMap::new();
            for (dst, ln, e) in g.edges.get(&f.id).map(|v| v.as_slice()).unwrap_or(&[]) {
                let short = dst.rsplit('.').next().unwrap_or(dst);
                by_line.entry((*ln, short)).or_default().push((dst, *e));
            }
            let callees: Vec<Value> = f
                .calls
                .iter()
                .map(|c| {
                    let hits = by_line.get(&(c.line, c.name.as_str())).map(|v| v.as_slice()).unwrap_or(&[]);
                    let name = if c.chain.is_empty() { c.name.clone() } else { format!("{}.{}", c.chain.join("."), c.name) };
                    json!({"name": name, "line": c.line, "targets": hits.iter().take(6).map(|h| h.0).collect::<Vec<_>>(),
                           "exact": hits.iter().any(|h| h.1)})
                })
                .collect();
            let reach = g.impact(std::slice::from_ref(&f.id), depth, false);
            let mut tests: Vec<&Func> = reach
                .iter()
                .filter(|(_, d)| **d > 0)
                .filter_map(|(t, _)| g.func(t))
                .filter(|t| t.kind == "test" || is_test(self.p, &t.file))
                .collect();
            tests.sort_by(|a, b| (&a.file, a.start, &a.id).cmp(&(&b.file, b.start, &b.id)));
            let mut source = src.iter().take(lines).cloned().collect::<Vec<_>>().join("\n");
            if src.len() > lines {
                source.push_str(&format!("\n... ({} more lines)", src.len() - lines));
            }
            out.push(json!({
                "id": f.id, "file": f.file, "start": f.start, "end": f.end, "source": source,
                "callers": callers.iter().map(|(e, file, ln, id)| json!({"id": id, "file": file, "line": ln, "exact": e})).collect::<Vec<_>>(),
                "callees": callees,
                "tests": tests.iter().map(|t| json!({"id": t.id, "file": t.file, "line": t.start})).collect::<Vec<_>>(),
            }));
        }
        out
    }

    /// Size, timing and, per language, how calls resolve; `project_exact`:
    /// of the calls that reach project code, the share that is exact (queries.stats).
    pub fn stats(&self) -> Value {
        let g = self.g;
        let mut langs: IndexMap<&str, usize> = IndexMap::new();
        for rel in g.by_file.keys() {
            *langs.entry(ext_of(rel)).or_insert(0) += 1;
        }
        let mut rows: Vec<(&String, &[usize; 4])> = g.resolution.iter().collect();
        rows.sort_by_key(|(_, r)| std::cmp::Reverse(r[0] + r[1] + r[2]));
        let mut res = Map::new();
        for (ext, r) in rows {
            let (ex, fz, no) = (r[0], r[1], r[2]);
            let pe = if ex + fz > 0 { json!(100 * ex / (ex + fz)) } else { Value::Null };
            res.insert(
                ext.clone(),
                json!({"calls": ex + fz + no, "exact": ex, "guess": fz, "nothing": no, "named": r[3], "project_exact": pe}),
            );
        }
        let edges: usize = g.edges.values().map(|v| v.len()).sum();
        let exact: usize = g.edges.values().map(|v| v.iter().filter(|e| e.2).count()).sum();
        json!({"root": self.p.root.to_string_lossy(), "files": self.p.files.len(), "parsed": g.by_file.len(),
               "languages": langs, "modules": g.modules.len(), "functions": g.funcs.len(), "edges": edges,
               "exact_edges": exact, "load_ms": g.load_ms as u64, "resolution": res})
    }
}
