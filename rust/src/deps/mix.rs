//! mix.exs (deps._mix): `defp deps` entries, the module namespaces and
//! Erlang modules each fetched package defines, who else needs one, and the
//! mix tasks scripts run.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use indexmap::IndexMap;

use super::fsx::{self, basename, dirname, glob, join, Liner};
use super::{dyn_rx, escape, find_after, lit, py_word, where_of, Ctx, Dep, Where};
use crate::pyre::Rx;

fn capitalize(p: &str) -> String {
    let mut c = p.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

pub fn read(ctx: &Ctx, mrel: &str) -> Vec<Dep> {
    static DEPS: OnceLock<Rx> = OnceLock::new();
    static DEP: OnceLock<Rx> = OnceLock::new();
    static ONLY: OnceLock<Rx> = OnceLock::new();
    static TEST: OnceLock<Rx> = OnceLock::new();
    static RUNTIME: OnceLock<Rx> = OnceLock::new();
    static DEFMOD: OnceLock<Rx> = OnceLock::new();
    static NEED: OnceLock<Rx> = OnceLock::new();
    static MODREF: OnceLock<Rx> = OnceLock::new();
    static ATOM: OnceLock<Rx> = OnceLock::new();
    static BODY_ATOM: OnceLock<Rx> = OnceLock::new();
    let mdir = dirname(mrel);
    let text = ctx.read(mrel);
    let Some(m) = lit(&DEPS, r"(?sm)defp?\s+deps\b.*?\bdo\b(.*?)^\s{2}end\b").search(&text) else { return Vec::new() };
    let (b0, b1) = m.span(1).unwrap();
    let (m0, m1) = (m.start(), m.end());
    let body = &text[b0..b1];
    let mut deps: IndexMap<String, Dep> = IndexMap::new();
    for d in lit(&DEP, r"\{\s*:(\w+)\s*,([^{}]*(?:\{[^{}]*\}[^{}]*)*)\}").find_iter(body) {
        let (name, opts) = (d.s(1), d.s(2));
        let mut scope = "runtime";
        if let Some(only) = lit(&ONLY, r"only:\s*(\[[^\]]*\]|:\w+)").search(opts) {
            if !only.s(1).contains(":prod") {
                let test = lit(&TEST, r"\[?\s*:test\s*\]?").fullmatch(crate::pyre::strip(only.s(1))).is_some();
                scope = if test { "test" } else { "dev" };
            }
        }
        if lit(&RUNTIME, r"runtime:\s*false").is_match(opts) && scope == "runtime" {
            scope = "tool";
        }
        deps.insert(name.to_string(), Dep::new("mix", name, mrel, fsx::line_of(&text, b0 + d.start()), scope));
    }
    let ddir =
        match std::env::var("MIX_DEPS_PATH").ok().filter(|v| !v.is_empty()).or_else(|| ctx.installed(mdir, "deps")) {
            Some(d) => d,
            None => {
                for dep in deps.values_mut() {
                    dep.uninstalled = true;
                }
                fsx::abs_join(ctx.root, &join(mdir, "deps"))
            }
        };
    // module namespaces each dep defines (the shortest ones), and erlang modules
    let mut ns_of: HashMap<String, String> = HashMap::new();
    let mut erl_of: IndexMap<String, String> = IndexMap::new();
    let names: Vec<String> = deps.keys().cloned().collect();
    for name in &names {
        let mut mods: HashSet<String> = HashSet::new();
        for f in glob(&join(&join(&join(&join(&ddir, name), "lib"), "**"), "*.ex")) {
            for m in lit(&DEFMOD, r"(?m)^\s*defmodule\s+([A-Z][\w.]*)").findall(&fsx::read(std::path::Path::new(&f))) {
                mods.insert(m.to_string());
            }
        }
        for f in glob(&join(&join(&join(&ddir, name), "src"), "*.erl")) {
            let b = basename(&f);
            erl_of.insert(b[..b.len() - 4].to_string(), name.clone());
        }
        if mods.is_empty() && !erl_of.values().any(|v| v == name) {
            deps[name].guessed = true;
            mods = [name.split('_').map(capitalize).collect::<String>()].into();
            erl_of.entry(name.clone()).or_insert_with(|| name.clone());
        }
        for m in &mods {
            if !mods.iter().any(|o| m.starts_with(&format!("{o}."))) {
                ns_of.entry(m.clone()).or_insert_with(|| name.clone());
            }
        }
    }
    // who else needs it: every fetched package's mix.exs, declared or not (a
    // path dep's dev tools are fetched too: sagents -> credo -> file_system).
    // An optional dep must be declared to be there (postgrex for ecto_sql); a
    // plain one comes along by itself; a package's `only:` deps usually don't.
    for f in glob(&join(&join(&ddir, "*"), "mix.exs")) {
        let other = basename(dirname(&f)).to_string();
        let t = fsx::read(std::path::Path::new(&f));
        for c in lit(&NEED, r"\{\s*:(\w+)\s*,([^}]*)\}").find_iter(&t) {
            let (need, opts) = (c.s(1), c.s(2));
            if need != other {
                if let Some(d) = deps.get_mut(need) {
                    if opts.contains("optional: true") {
                        d.needed_by.push(other.clone());
                    } else if !opts.contains("only:") {
                        d.via.push(other.clone());
                    }
                }
            }
        }
    }
    let modref = lit(&MODREF, r"([A-Z]\w*(?:\.[A-Z]\w*)*)"); // (?<![\w.])
    let atom = lit(&ATOM, r":(\w+)\b"); // (?<![\w:])
    let not_ex = |c: char| py_word(c) || c == '.';
    let not_atom = |c: char| py_word(c) || c == ':';
    let mdir_or_dot = if mdir.is_empty() { "." } else { mdir };
    for rel in ctx.walk(mdir, &[".ex", ".exs", ".heex", ".eex", ".leex"], &["mix.exs"]) {
        let raw = ctx.read(&rel);
        let code = ctx.code(&rel, &raw, "elixir");
        let lof = Liner::new(&code);
        let w = where_of(&fsx::relpath(&rel, mdir_or_dot));
        let mut seen: HashSet<(String, Where)> = HashSet::new();
        let mut seen_atom: HashSet<(String, Where)> = HashSet::new();
        for r in find_after(modref, &code, not_ex, |_| true, None) {
            let parts: Vec<&str> = r.s(1).split('.').collect();
            for k in (1..=parts.len()).rev() {
                if let Some(dn) = ns_of.get(&parts[..k].join(".")) {
                    if !seen.contains(&(dn.clone(), w)) || w == Where::Code {
                        deps[dn].add(&rel, lof.line(r.start()), Some(w));
                        seen.insert((dn.clone(), w));
                    }
                    break;
                }
            }
        }
        let src = if w != Where::Config { &code } else { &raw };
        for r in find_after(atom, src, not_atom, |_| true, None) {
            let a = r.s(1);
            let mut dn = if code.as_bytes().get(r.end()) == Some(&b'.') { erl_of.get(a).cloned() } else { None };
            if dn.is_none() && w == Where::Config && deps.contains_key(a) && rel != mrel {
                dn = Some(a.to_string()); // `config :tailwind, ...`, `compilers: [:phoenix_live_view]`
            }
            if let Some(dn) = dn {
                if seen_atom.insert((dn.clone(), w)) {
                    deps[&dn].add(&rel, lof.line(r.start()), Some(w));
                }
            }
        }
        if rel == mrel {
            // the manifest's own aliases / compilers / listeners (atoms)
            let rest = format!("{}{}", &code[..m0.min(code.len())], &code[m1.min(code.len())..]);
            let mut hit: Vec<String> = Vec::new();
            for c in find_after(lit(&BODY_ATOM, r":(\w+)"), &rest, not_atom, |_| true, None) {
                let a = c.s(1);
                if deps.contains_key(a) && !hit.iter().any(|h| h == a) {
                    hit.push(a.to_string());
                }
            }
            for a in hit {
                deps[&a].add(&rel, 1, Some(Where::Config));
            }
        }
    }
    // assets: tailwind `@source "../../deps/heroicons"`, `@plugin`, JS `from "phoenix"` (mix-shipped JS)
    let mut sorted: Vec<String> = names.iter().map(|n| escape(n)).collect();
    sorted.sort_by_key(|n| std::cmp::Reverse(n.chars().count()));
    let alts = sorted.join("|");
    let asset = dyn_rx(&format!(r#"deps/({alts})\b|["']({alts})["'/]"#));
    for rel in ctx.walk(mdir, &[".css", ".js", ".ts", ".mjs"], &["mix.exs", "package.json"]) {
        let raw = ctx.read(&rel);
        let mut hit: Vec<String> = Vec::new();
        for c in asset.find_iter(&raw) {
            let dn = if c.s(1).is_empty() { c.s(2) } else { c.s(1) };
            if !hit.iter().any(|h| h == dn) {
                hit.push(dn.to_string());
            }
        }
        for dn in hit {
            // (an empty name: a mix.exs whose deps list is empty matched `""`; Python fails here)
            if let Some(d) = deps.get_mut(&dn) {
                d.add(&rel, 1, if rel.ends_with(".css") { Some(Where::Config) } else { None });
            }
        }
    }
    // dev tools run as mix tasks: `mix dialyzer` in a script / CI step / alias
    let runs: String = ctx
        .walk(mdir, &[".sh", ".yml", ".yaml", "Makefile", "justfile", "mix.exs"], &["mix.exs"])
        .iter()
        .map(|r| ctx.read(r))
        .collect::<Vec<_>>()
        .join("\n");
    for name in &names {
        for t in glob(&join(&join(&join(&join(&join(&ddir, name), "lib"), "mix"), "tasks"), "*.ex")) {
            let b = basename(&t);
            let task = &b[..b.len() - 3];
            if dyn_rx(&format!(r#"(?:\bmix\s+|["']){}\b"#, escape(task))).is_match(&runs) {
                let d = &mut deps[name];
                let line = d.line;
                d.add(mrel, line, Some(Where::Config));
                break;
            }
        }
    }
    deps.into_values().collect()
}
