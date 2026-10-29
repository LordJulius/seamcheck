//! Import resolution (core._resolve_where and its per-language readers, and
//! the passes of core._resolve_imports around it).

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};

use serde_json::{Map, Value};

use crate::graph::infer::{re, rxd};
use crate::graph::Graph;
use crate::lang::ruby::link::normpath;
use crate::project::Project;

/// _resolve_where's answer for an import from outside the project.
pub const EXTERNAL: &str = "ext:";

fn join(a: &str, b: &str) -> String {
    if b.starts_with('/') || a.is_empty() {
        b.to_string()
    } else {
        format!("{}/{}", a.trim_end_matches('/'), b)
    }
}

fn dirname(p: &str) -> &str {
    p.rsplit_once('/').map(|x| x.0).unwrap_or("")
}

/// `os.path.relpath(target, root)` for two normalized absolute paths.
fn relpath(target: &str, root: &str) -> String {
    let t: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
    let r: Vec<&str> = root.split('/').filter(|s| !s.is_empty()).collect();
    let mut k = 0;
    while k < t.len() && k < r.len() && t[k] == r[k] {
        k += 1;
    }
    let mut out: Vec<String> = vec!["..".into(); r.len() - k];
    out.extend(t[k..].iter().map(|s| s.to_string()));
    if out.is_empty() {
        ".".into()
    } else {
        out.join("/")
    }
}

/// compilerOptions of a tsconfig, its `extends` chain merged in (the base's
/// `paths` count unless this file sets its own) (core._read_tsconfig).
fn read_tsconfig(path: &str, depth: usize) -> Map<String, Value> {
    let Ok(bytes) = std::fs::read(path) else { return Map::new() };
    let txt = String::from_utf8_lossy(&bytes).into_owned();
    let cfg: Value = match serde_json::from_str(&txt) {
        Ok(v) => v,
        Err(_) => {
            // tsconfig allows comments and trailing commas
            let t = re!(r"(?m)^\s*//.*$").sub("", &txt);
            match serde_json::from_str(&re!(r",(\s*[}\]])").sub(r"\1", &t)) {
                Ok(v) => v,
                Err(_) => return Map::new(),
            }
        }
    };
    let Some(cfg) = cfg.as_object() else { return Map::new() };
    let mut opts: Map<String, Value> = cfg.get("compilerOptions").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let here = dirname(path).to_string();
    if let Some(b) = opts.get("baseUrl").and_then(|v| v.as_str()) {
        opts.insert("baseUrl".into(), Value::String(normpath(&join(&here, b))));
    }
    if let Some(ext) = cfg.get("extends").and_then(|v| v.as_str()) {
        if ext.starts_with('.') && depth < 4 {
            let mut base_path = normpath(&join(&here, ext));
            if !base_path.ends_with(".json") {
                base_path.push_str(".json");
            }
            let base = read_tsconfig(&base_path, depth + 1);
            for (k, v) in &base {
                opts.entry(k.clone()).or_insert(v.clone());
            }
            let pd = base.get("\0paths_dir").cloned().unwrap_or_else(|| Value::String(dirname(&base_path).to_string()));
            opts.entry("\0paths_dir".to_string()).or_insert(pd);
        }
    }
    if cfg.get("compilerOptions").and_then(|c| c.get("paths")).is_some() {
        opts.insert("\0paths_dir".into(), Value::String(here));
    }
    opts
}

type TsPaths = Vec<(String, String, bool)>;

pub struct Imports<'a> {
    g: &'a Graph,
    p: &'a Project,
    manifests: RefCell<HashMap<String, Vec<String>>>,
    tsconfig: RefCell<HashMap<String, TsPaths>>,
    workspaces: RefCell<Option<Vec<(String, (String, String))>>>,
    pkgmain: RefCell<HashMap<String, String>>,
    gomod: RefCell<Option<(Vec<(String, String)>, HashMap<String, Vec<String>>)>>,
    slint: RefCell<Option<HashMap<String, Vec<String>>>>,
    inc: RefCell<Option<HashMap<String, Vec<String>>>>,
    rs: RefCell<Option<(Vec<(String, String)>, Vec<(String, String)>)>>,
}

impl<'a> Imports<'a> {
    pub fn new(g: &'a Graph, p: &'a Project) -> Imports<'a> {
        Imports {
            g,
            p,
            manifests: Default::default(),
            tsconfig: Default::default(),
            workspaces: Default::default(),
            pkgmain: Default::default(),
            gomod: Default::default(),
            slint: Default::default(),
            inc: Default::default(),
            rs: Default::default(),
        }
    }

    /// Every `name` (package.json, go.mod, Cargo.toml) in the repo, whether or
    /// not `[project] include` takes it in: they say how imports resolve.
    fn manifests(&self, name: &str) -> Vec<String> {
        if let Some(h) = self.manifests.borrow().get(name) {
            return h.clone();
        }
        let proj = self.p.config.table("project");
        let include = crate::config::list_or(proj, "include", &["**/*"]);
        let mut exclude = crate::walk::default_exclude(&include);
        exclude.extend(crate::config::list_or(proj, "exclude", &[]));
        let found = crate::walk::list_files(&self.p.root, &[name.to_string(), format!("**/{name}")], &exclude);
        self.manifests.borrow_mut().insert(name.to_string(), found.clone());
        found
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read(self.p.root.join(rel)).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    /// (key, target, wildcard) from the nearest tsconfig.json's `paths`:
    /// `"@/*": ["src/*"]` is a prefix, `"@app/common": ["packages/common/src/index.ts"]`
    /// one exact name.
    fn tsconfig_paths(&self, d: &str) -> TsPaths {
        if let Some(h) = self.tsconfig.borrow().get(d) {
            return h.clone();
        }
        let mut found: TsPaths = Vec::new();
        let p = self.p.root.join(d).join("tsconfig.json");
        if p.is_file() {
            let ps = p.to_string_lossy().into_owned();
            let opts = read_tsconfig(&ps, 0);
            let root = self.p.root.to_string_lossy().into_owned();
            let root = normpath(&root);
            let base = opts
                .get("baseUrl")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| opts.get("\0paths_dir").and_then(|v| v.as_str()).filter(|s| !s.is_empty()))
                .map(|s| s.to_string())
                .unwrap_or_else(|| dirname(&ps).to_string());
            if let Some(paths) = opts.get("paths").and_then(|v| v.as_object()) {
                for (k, vs) in paths {
                    let Some(v0) = vs.as_array().and_then(|a| a.first()).and_then(|x| x.as_str()) else { continue };
                    let target = relpath(&normpath(&join(&base, v0.trim_end_matches('*'))), &root);
                    if k.ends_with('*') && v0.ends_with('*') {
                        let t = if v0[..v0.len() - 1].ends_with('/') { format!("{}/", target.trim_end_matches('/')) } else { target };
                        found.push((k[..k.len() - 1].to_string(), t, true));
                    } else if !k.contains('*') {
                        found.push((k.clone(), target, false));
                    }
                }
            }
        } else if !d.is_empty() {
            found = self.tsconfig_paths(dirname(d));
        }
        self.tsconfig.borrow_mut().insert(d.to_string(), found.clone());
        found
    }

    /// A module name for an adapter's "js:<dir>\0<spec>" / "py:<dir>\0<spec>"
    /// (anything else unchanged). "" when it can't be found, EXTERNAL when it
    /// names a package from outside the project (node_modules, the stdlib).
    pub fn resolve_where(&self, py: &crate::graph::pylink::PyIndex, where_: &str) -> String {
        if where_.starts_with("py:") {
            return py.resolve(self.g, where_);
        }
        if where_.starts_with("rs:") {
            return self.resolve_rs(where_);
        }
        if where_.starts_with("inc:") {
            return self.resolve_include(where_);
        }
        if where_.starts_with("sl:") {
            return self.resolve_slint(where_);
        }
        if where_.starts_with("go:") || where_.starts_with("go?:") {
            return self.resolve_go(where_.split_once(':').unwrap().1);
        }
        let Some(rest) = where_.strip_prefix("js:") else { return where_.to_string() };
        let (d, spec) = rest.split_once('\0').unwrap_or((rest, ""));
        let g = self.g;
        let base: String;
        if spec.starts_with('.') {
            base = normpath(&join(d, spec));
        } else {
            let mut b = String::new();
            for (pre, t, wild) in self.tsconfig_paths(d) {
                if if wild { spec.starts_with(&pre) } else { spec == pre } {
                    b = if wild { format!("{t}{}", &spec[pre.len()..]) } else { t };
                    break;
                }
            }
            if b.is_empty() {
                b = self.workspace_entry(spec);
            }
            if b.is_empty() {
                return EXTERNAL.into(); // a package from node_modules
            }
            base = b;
        }
        let base = re!(r"\.(?:[mc]?[jt]sx?)$").sub("", base.trim_end_matches('/'));
        let base = if base == "." { String::new() } else { base };
        let cands: Vec<String> = if base.is_empty() {
            vec!["index".into(), "src/index".into()]
        } else {
            vec![base.clone(), format!("{base}/index"), format!("{base}/src/index")]
        };
        for cand in cands {
            if g.modules.contains_key(&cand) {
                return cand;
            }
        }
        let main = self.package_main(&base); // `require('../..')`: the folder's package.json "main"
        if g.modules.contains_key(&main) {
            main
        } else {
            String::new()
        }
    }

    /// The module a folder's package.json names as its entry ("main", or the
    /// "." export's require / import / default), "" if none.
    fn package_main(&self, d: &str) -> String {
        if let Some(h) = self.pkgmain.borrow().get(d) {
            return h.clone();
        }
        let mut got = String::new();
        let pkg: Option<Value> = std::fs::read(self.p.root.join(d).join("package.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
        if let Some(pkg) = pkg.as_ref().and_then(|p| p.as_object()) {
            let mut ent: Vec<Option<Value>> = vec![pkg.get("main").cloned(), pkg.get("module").cloned()];
            let mut ex = pkg.get("exports").cloned();
            if let Some(Value::Object(o)) = &ex {
                if let Some(dot) = o.get(".") {
                    ex = Some(dot.clone());
                }
            }
            while let Some(Value::Object(o)) = &ex {
                // conditions: the first plain target
                ex = ["require", "import", "node", "default"].iter().find_map(|k| o.get(*k).cloned());
            }
            ent.push(ex);
            for e in ent.into_iter().flatten() {
                if let Some(s) = e.as_str() {
                    let m = re!(r"\.(?:[mc]?[jt]sx?)$").sub("", &normpath(&join(d, s)));
                    if self.g.modules.contains_key(&m) {
                        got = m;
                        break;
                    }
                }
            }
        }
        self.pkgmain.borrow_mut().insert(d.to_string(), got.clone());
        got
    }

    /// `@scope/pkg` or `@scope/pkg/sub` -> a path in the project, for monorepo
    /// packages named in their package.json (npm / yarn / pnpm workspaces).
    fn workspace_entry(&self, spec: &str) -> String {
        if self.workspaces.borrow().is_none() {
            let mut ws: Vec<(String, (String, String))> = Vec::new();
            for rel in self.manifests("package.json") {
                if rel.rsplit('/').next() == Some("package.json") && !format!("/{rel}").contains("/node_modules/") {
                    let Some(pkg) = self.read(&rel).and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { continue };
                    if let Some(name) = pkg.get("name").and_then(|n| n.as_str()) {
                        let d = dirname(&rel).to_string();
                        let main = ["source", "main", "module"]
                            .iter()
                            .find_map(|k| pkg.get(*k).filter(|v| !v.is_null() && v.as_str() != Some("")))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        match ws.iter().position(|(n, _)| n == name) {
                            Some(k) => ws[k].1 = (d, main),
                            None => ws.push((name.to_string(), (d, main))),
                        }
                    }
                }
            }
            *self.workspaces.borrow_mut() = Some(ws);
        }
        let ws = self.workspaces.borrow().clone().unwrap();
        let strip = |c: &str| re!(r"\.(?:[mc]?[jt]sx?)$").sub("", c);
        for (name, (d, main)) in &ws {
            if spec == name {
                let entry = if !main.is_empty()
                    && !["dist", "./dist", "build", "./build", "lib/", "./lib/"].iter().any(|p| main.starts_with(p))
                {
                    normpath(&join(d, main))
                } else {
                    String::new()
                };
                let mut cands: Vec<String> = if entry.is_empty() { vec![] } else { vec![entry] };
                cands.extend(["src/index", "source/index", "lib/index", "index"].iter().map(|x| join(d, x)));
                for cand in cands {
                    if self.g.modules.contains_key(&strip(&cand)) {
                        return cand;
                    }
                }
                return join(d, "src/index");
            }
            if let Some(rest) = spec.strip_prefix(&format!("{name}/")) {
                for cand in [join(&join(d, "src"), rest), join(d, rest)] {
                    let c2 = strip(&cand);
                    if self.g.modules.contains_key(&c2) || self.g.modules.contains_key(&format!("{c2}/index")) {
                        return cand;
                    }
                }
                return join(d, rest);
            }
        }
        String::new()
    }

    /// A Go import path -> "pkg:<dir>#<name>" through the go.mod files'
    /// module paths; EXTERNAL for the standard library and other modules.
    fn resolve_go(&self, path: &str) -> String {
        if self.gomod.borrow().is_none() {
            let mut mods: Vec<(String, String)> = Vec::new();
            for rel in self.manifests("go.mod") {
                if rel == "go.mod" || rel.ends_with("/go.mod") {
                    let Some(t) = self.read(&rel) else { continue };
                    if let Some(mm) = re!(r"(?m)^module\s+(\S+)").search(&t) {
                        mods.push((mm.s(1).trim_matches('"').to_string(), dirname(&rel).to_string()));
                    }
                }
            }
            mods.sort_by_key(|x| std::cmp::Reverse(x.0.len())); // the most specific module first
            let mut by_dir: HashMap<String, Vec<String>> = HashMap::new();
            for key in self.g.packages.keys() {
                if let Some((d, _)) = key.rsplit_once('#') {
                    by_dir.entry(d.to_string()).or_default().push(key.clone());
                }
            }
            *self.gomod.borrow_mut() = Some((mods, by_dir));
        }
        let gm = self.gomod.borrow();
        let (mods, by_dir) = gm.as_ref().unwrap();
        for (mpath, mdir) in mods {
            if path == mpath || path.starts_with(&format!("{mpath}/")) {
                let d = if path != mpath { normpath(&join(mdir, path[mpath.len()..].trim_start_matches('/'))) } else { mdir.clone() };
                let d = if d == "." { String::new() } else { d };
                let keys: Vec<&String> = by_dir.get(&d).map(|v| v.iter().filter(|k| !k.ends_with("_test")).collect()).unwrap_or_default();
                return if keys.len() == 1 { format!("pkg:{}", keys[0]) } else { String::new() };
            }
        }
        EXTERNAL.into()
    }

    /// A Slint `import {..} from "x.slint"`: beside the file, else the one
    /// project file whose path ends with it (`@lib/x.slint` names a library path
    /// the build registers: its tail); std-widgets and other missing files are
    /// the toolkit's.
    fn resolve_slint(&self, where_: &str) -> String {
        let (d, spec) = where_[3..].split_once('\0').unwrap_or((&where_[3..], ""));
        let g = self.g;
        if spec.starts_with('.') {
            let base = re!(r"\.slint$").sub("", &normpath(&join(d, spec)));
            return if g.modules.contains_key(&base) { base } else { EXTERNAL.into() };
        }
        let t0 = if spec.starts_with('@') && spec.contains('/') { spec.split_once('/').unwrap().1 } else { spec };
        let tail = re!(r"\.slint$").sub("", t0);
        let base = normpath(&join(d, &tail));
        if g.modules.contains_key(&base) {
            return base;
        }
        if self.slint.borrow().is_none() {
            let mut idx: HashMap<String, Vec<String>> = HashMap::new();
            for m in g.modules.values() {
                if m.file.ends_with(".slint") {
                    let parts: Vec<&str> = m.name.split('/').collect();
                    for k in 0..parts.len() {
                        idx.entry(parts[k..].join("/")).or_default().push(m.name.clone());
                    }
                }
            }
            *self.slint.borrow_mut() = Some(idx);
        }
        let s = self.slint.borrow();
        let hits = s.as_ref().unwrap().get(&tail).cloned().unwrap_or_default();
        if hits.len() == 1 {
            hits[0].clone()
        } else if hits.is_empty() {
            EXTERNAL.into()
        } else {
            String::new()
        }
    }

    /// A C/C++ `#include "x.h"`: beside the file, else the one project file
    /// whose path ends with it (an include directory).
    fn resolve_include(&self, where_: &str) -> String {
        let (d, spec) = where_[4..].split_once('\0').unwrap_or((&where_[4..], ""));
        let g = self.g;
        let base = normpath(&join(d, spec));
        if let Some(m) = g.by_file.get(&base) {
            return m[0].clone();
        }
        if self.inc.borrow().is_none() {
            let mut idx: HashMap<String, Vec<String>> = HashMap::new();
            for rel in g.by_file.keys() {
                if [".h", ".hpp", ".hh", ".hxx"].iter().any(|e| rel.ends_with(e)) {
                    let parts: Vec<&str> = rel.split('/').collect();
                    for k in 0..parts.len() {
                        idx.entry(parts[k..].join("/")).or_default().push(rel.clone());
                    }
                }
            }
            *self.inc.borrow_mut() = Some(idx);
        }
        let s = self.inc.borrow();
        let hits = s.as_ref().unwrap().get(spec.trim_start_matches(['.', '/'])).cloned().unwrap_or_default();
        if hits.len() == 1 {
            return g.by_file[&hits[0]][0].clone();
        }
        if hits.is_empty() {
            EXTERNAL.into()
        } else {
            String::new() // a system / library header
        }
    }

    /// Rust crates: [(crate dir, src dir)] deepest first, and name -> src dir.
    fn rs_crates(&self) -> (Vec<(String, String)>, Vec<(String, String)>) {
        if let Some(h) = self.rs.borrow().as_ref() {
            return h.clone();
        }
        let (mut dirs, mut names): (Vec<(String, String)>, Vec<(String, String)>) = (vec![], vec![]);
        for rel in self.manifests("Cargo.toml") {
            if rel != "Cargo.toml" && !rel.ends_with("/Cargo.toml") {
                continue;
            }
            let d = dirname(&rel).to_string();
            let src = if d.is_empty() { "src".to_string() } else { format!("{d}/src") };
            let txt = self.read(&rel).unwrap_or_default();
            // `[lib] path = "x/lib.rs"` / `[[bin]] path = "crates/core/main.rs"`: the crate root is there
            for r in re!(r#"(?ms)^\[\[?(?:lib|bin)\]\]?[^\[]*?^\s*path\s*=\s*"([^"]+)""#).find_iter(&txt) {
                let rd = normpath(&join(&d, dirname(r.s(1))));
                dirs.push((rd.clone(), rd));
            }
            dirs.push((d.clone(), src.clone()));
            for sect in ["package", "lib"] {
                if let Some(mm) = rxd(&format!(r#"(?ms)^\[{sect}\][^\[]*?^\s*name\s*=\s*"([^"]+)""#)).search(&txt) {
                    let lib = re!(r#"(?ms)^\[lib\][^\[]*?^\s*path\s*=\s*"([^"]+)""#).search(&txt);
                    let val = match lib {
                        Some(l) => normpath(&join(&d, dirname(l.s(1)))),
                        None => src.clone(),
                    };
                    let k = mm.s(1).replace('-', "_");
                    names.retain(|(n, _)| *n != k);
                    names.push((k, val));
                }
            }
        }
        dirs.sort_by_key(|x| std::cmp::Reverse(x.0.len()));
        *self.rs.borrow_mut() = Some((dirs.clone(), names.clone()));
        (dirs, names)
    }

    /// The file module for a module path in a crate ("" when there is none).
    fn rs_module(&self, src: &str, parts: &[String]) -> String {
        let g = self.g;
        if parts.is_empty() {
            for root in ["lib", "main"] {
                let c = format!("{src}/{root}");
                if g.modules.contains_key(&c) {
                    return c;
                }
            }
            return String::new();
        }
        let base = format!("{src}/{}", parts.join("/"));
        for cand in [base.clone(), format!("{base}/mod")] {
            if g.modules.contains_key(&cand) {
                return cand;
            }
        }
        String::new()
    }

    /// A Rust `use` path -> the module it names, or the module holding the item
    /// it names. `crate::` / `self::` / `super::` / a workspace crate's name; any
    /// other first segment is a child module, else another crate (EXTERNAL).
    fn resolve_rs(&self, where_: &str) -> String {
        let (rel, path) = where_[3..].split_once('\0').unwrap_or((&where_[3..], ""));
        let mut segs: Vec<String> = if path.is_empty() { vec![] } else { path.split("::").map(|s| s.to_string()).collect() };
        let (dirs, names) = self.rs_crates();
        let mut src: String = dirs.iter().find(|(_, s)| rel.starts_with(&format!("{s}/"))).map(|x| x.1.clone()).unwrap_or_default();
        if let Some(bm) = re!(r"(.*?/?src/bin)/([^/]+)/").match_start(rel) {
            if !src.is_empty() && rel.starts_with(&format!("{src}/bin/")) {
                src = format!("{}/{}", bm.s(1), bm.s(2)); // `src/bin/bat/main.rs`: a binary crate rooted there
            }
        }
        let mut cur: Vec<String> = vec![];
        if !src.is_empty() {
            let p = &rel[src.len() + 1..rel.len() - 3]; // (.rs)
            cur = if p == "lib" || p == "main" {
                vec![]
            } else if let Some(s) = p.strip_suffix("/mod") {
                s.split('/').map(|x| x.to_string()).collect()
            } else {
                p.split('/').map(|x| x.to_string()).collect()
            };
            if p.ends_with("/mod") && (&p[..p.len() - 4] == "" || &p[..p.len() - 4] == "lib") {
                cur = vec![];
            }
        }
        if segs.is_empty() {
            return String::new();
        }
        let base: Vec<String>;
        if segs[0] == "crate" {
            base = vec![];
            segs.remove(0);
        } else if segs[0] == "self" || segs[0] == "super" {
            let mut b = cur.clone();
            while !segs.is_empty() && (segs[0] == "self" || segs[0] == "super") {
                if segs[0] == "super" {
                    b.pop();
                }
                segs.remove(0);
            }
            base = b;
        } else if let Some((_, s)) = names.iter().find(|(n, _)| *n == segs[0]) {
            src = s.clone();
            base = vec![];
            segs.remove(0);
        } else if !src.is_empty() && !self.rs_module(&src, &[cur.clone(), vec![segs[0].clone()]].concat()).is_empty() {
            base = cur.clone();
        } else if !src.is_empty() && !self.rs_module(&src, &[segs[0].clone()]).is_empty() {
            base = vec![]; // (2015 edition: paths start at the crate root)
        } else {
            return EXTERNAL.into(); // std, core, alloc or a dependency
        }
        if src.is_empty() {
            return String::new();
        }
        let mut best = self.rs_module(&src, &base);
        for k in 0..segs.len() {
            let m = self.rs_module(&src, &[base.clone(), segs[..=k].to_vec()].concat());
            if m.is_empty() {
                break;
            }
            best = m;
        }
        best
    }
}

/// `okhttp3.TestUtil` / `okhttp3.CipherSuite.Companion`: the module of type
/// TestUtil (CipherSuite) in package okhttp3, or "".
pub fn jvm_member_home(g: &Graph, path: &str) -> String {
    let (mut pkg, mut typ) = match path.rsplit_once('.') {
        Some((a, b)) => (a, b),
        None => ("", path),
    };
    if typ == "Companion" {
        (pkg, typ) = match pkg.rsplit_once('.') {
            Some((a, b)) => (a, b),
            None => ("", pkg),
        };
    }
    for mn in g.packages.get(pkg).map(|v| v.as_slice()).unwrap_or(&[]) {
        let m = &g.modules[mn];
        if m.decls.iter().any(|d| d == typ) {
            let inner = format!("{mn}.{typ}");
            return if g.modules.contains_key(&inner) { inner } else { mn.clone() };
        }
    }
    String::new()
}

#[allow(dead_code)]
fn _unused(_: BTreeSet<String>) {}
