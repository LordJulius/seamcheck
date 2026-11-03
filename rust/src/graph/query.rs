//! The graph's own queries (core.Project.find, core.reach, core.path,
//! core.edge_split, core.impact, core.dead): what `queries` builds its
//! answers from.

use std::collections::{HashMap, HashSet, VecDeque};

use indexmap::IndexMap;

use crate::glob::{fnmatchcase, glob_re};
use crate::graph::Graph;
use crate::model::Func;
use crate::pyre::Rx;

fn rx(k: usize, pat: &'static str) -> &'static Rx {
    static CELLS: [std::sync::OnceLock<Rx>; 2] = [const { std::sync::OnceLock::new() }; 2];
    CELLS[k].get_or_init(|| Rx::lit(pat))
}

/// Names the engine or a framework calls by convention (core.ENGINE_CALLBACKS).
pub fn engine_callback(name: &str) -> bool {
    rx(
        1,
        r"^(_ready|_process|_physics_process|_input|_unhandled_input|_gui_input|_draw|_init|_enter_tree|_exit_tree|_notification|_to_string|_get|_set|_on_\w+|handle_\w+|init|mount|render|main|__\w+__|setup|test_\w+|call|start_link|child_spec|terminate|code_change)$",
    )
    .match_start(name)
    .is_some()
}

impl Graph {
    /// Every function in id order (Python's `funcs.values()`).
    pub fn all_funcs(&self) -> impl Iterator<Item = &Func> {
        self.funcs.keys().filter_map(|id| self.func(id))
    }

    /// An id as `find` prints it, 'name' (glob ok), 'Module.name',
    /// 'path/glob:funcglob', or 'path/file.ext:LINE' (the function holding
    /// that line) (core.Project.find).
    pub fn find(&self, spec: &str) -> Vec<&Func> {
        if let Some(f) = self.func(spec) {
            return vec![f];
        }
        if let Some(lm) = rx(0, r"^(.+?\.\w+):(\d+)$").match_start(spec) {
            let file = lm.s(1);
            if self.by_file.contains_key(file) {
                // (Python's int() reads any Unicode digit; a line number past usize is no function's)
                let ln: usize = lm.s(2).parse().unwrap_or(usize::MAX);
                let mut held: Vec<&Func> =
                    self.all_funcs().filter(|f| f.file == file && f.start <= ln && ln <= f.end && f.kind != "top").collect();
                held.sort_by_key(|f| f.end as i64 - f.start as i64);
                held.truncate(1);
                return held;
            }
        }
        if let Some((pg, fg)) = spec.split_once(':') {
            let re = glob_re(pg);
            let fg = if fg.is_empty() { "*" } else { fg };
            return self.all_funcs().filter(|f| re.is_match(&f.file) && fnmatchcase(&f.name, fg)).collect();
        }
        if spec.contains('/') {
            let re = glob_re(spec);
            return self.all_funcs().filter(|f| re.is_match(&f.file)).collect();
        }
        if let Some((m, name)) = spec.rsplit_once('.') {
            let (dot, slash) = (format!(".{m}"), format!("/{m}"));
            return self
                .all_funcs()
                .filter(|f| {
                    (f.module == m || f.module.ends_with(&dot) || f.module.ends_with(&slash)) && fnmatchcase(&f.name, name)
                })
                .collect();
        }
        self.all_funcs().filter(|f| fnmatchcase(&f.name, spec) && f.kind != "top").collect()
    }

    /// Each function within `depth` calls of `start` and its distance (core.reach).
    pub fn reach(&self, start: &str, depth: usize, reverse: bool, exact_only: bool) -> IndexMap<String, usize> {
        let graph = if reverse { &self.redges } else { &self.edges };
        let mut dist: IndexMap<String, usize> = IndexMap::new();
        dist.insert(start.to_string(), 0);
        let mut dq: VecDeque<String> = VecDeque::from([start.to_string()]);
        while let Some(u) = dq.pop_front() {
            let du = dist[&u];
            if du >= depth {
                continue;
            }
            for (v, _, exact) in graph.get(&u).map(|v| v.as_slice()).unwrap_or(&[]) {
                if exact_only && !exact {
                    continue;
                }
                if !dist.contains_key(v) {
                    dist.insert(v.clone(), du + 1);
                    dq.push_back(v.clone());
                }
            }
        }
        dist
    }

    /// The shortest call chain from `a` to `b`, or empty (core.path).
    pub fn path(&self, a: &str, b: &str, max_depth: usize, exact_only: bool) -> Vec<String> {
        let mut prev: HashMap<String, Option<String>> = HashMap::new();
        prev.insert(a.to_string(), None);
        let mut dq: VecDeque<(String, usize)> = VecDeque::from([(a.to_string(), 0)]);
        while let Some((u, d)) = dq.pop_front() {
            if u == b {
                let mut out = vec![u.clone()];
                let mut cur = prev[&u].clone();
                // (Python's `while u:` stops at an empty id too)
                while let Some(x) = cur.filter(|x| !x.is_empty()) {
                    cur = prev[&x].clone();
                    out.push(x);
                }
                out.reverse();
                return out;
            }
            if d >= max_depth {
                continue;
            }
            for (v, _, exact) in self.edges.get(&u).map(|v| v.as_slice()).unwrap_or(&[]) {
                if exact_only && !exact {
                    continue;
                }
                if !prev.contains_key(v) {
                    prev.insert(v.clone(), Some(u.clone()));
                    dq.push_back((v.clone(), d + 1));
                }
            }
        }
        Vec::new()
    }

    /// (exact, name-guess) distinct callers (or callees) of one function (core.edge_split).
    pub fn edge_split(&self, fid: &str, reverse: bool) -> (usize, usize) {
        let graph = if reverse { &self.redges } else { &self.edges };
        let (mut ex, mut guess): (HashSet<&str>, HashSet<&str>) = Default::default();
        for (v, _, exact) in graph.get(fid).map(|v| v.as_slice()).unwrap_or(&[]) {
            if *exact {
                ex.insert(v);
            } else {
                guess.insert(v);
            }
        }
        (ex.len(), guess.iter().filter(|v| !ex.contains(*v)).count())
    }

    /// Everything a change to `start` can reach: callers, transitively, plus
    /// what bridges carry forward; distance per function (core.impact).
    pub fn impact(&self, start: &[String], depth: usize, fuzzy: bool) -> IndexMap<String, usize> {
        let mut dist: IndexMap<String, usize> = start.iter().map(|s| (s.clone(), 0)).collect();
        let mut dq: VecDeque<String> = start.iter().cloned().collect();
        while let Some(u) = dq.pop_front() {
            let du = dist[&u];
            if du >= depth {
                continue;
            }
            // certain edges only unless asked: a name guess is not "this breaks"
            let mut nxt: Vec<&String> = self
                .redges
                .get(&u)
                .map(|v| v.iter().filter(|e| e.2 || fuzzy).map(|e| &e.0).collect())
                .unwrap_or_default();
            if let Some(out) = self.edges.get(&u) {
                nxt.extend(out.iter().filter(|e| self.bridged.contains_key(&(u.clone(), e.0.clone()))).map(|e| &e.0));
            }
            for v in nxt {
                if !dist.contains_key(v) && self.funcs.contains_key(v) {
                    dist.insert(v.clone(), du + 1);
                    dq.push_back(v.clone());
                }
            }
        }
        dist
    }

    /// Functions nothing calls or references (core.dead), in id order.
    pub fn dead(&self) -> Vec<&Func> {
        let mut called: HashSet<&str> = HashSet::new();
        let mut refs: HashSet<&str> = HashSet::new();
        for f in self.all_funcs() {
            refs.extend(f.refs.iter().map(|s| s.as_str()));
            for (t, _, _) in self.edges.get(&f.id).map(|v| v.as_slice()).unwrap_or(&[]) {
                called.insert(t);
            }
        }
        self.all_funcs()
            .filter(|f| {
                !(f.kind == "top"
                    || f.kind == "test"
                    || called.contains(f.id.as_str())
                    || refs.contains(f.name.as_str())
                    || engine_callback(&f.name))
            })
            .collect()
    }
}
