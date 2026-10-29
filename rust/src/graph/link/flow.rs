//! Parameter flow (core._param_flow, _param_receivers, _passed, _arg_value):
//! a call through a parameter reaches what every caller passes there.

use indexmap::IndexMap;

use super::call::Resolver;
use crate::graph::infer::{ends, re, Val};
use crate::graph::Edge;
use crate::lang::chains::expression;
use crate::lang::lambdas::{matching, split_top};
use crate::model::{Call, Func};

pub(super) type Edges = IndexMap<String, Vec<Edge>>;

#[derive(Clone, PartialEq, Eq, Debug)]
enum ArgVal {
    Ext,
    Inst(Vec<String>),
}

fn remove_edge(edges: &mut Edges, from: &str, e: &Edge) {
    if let Some(v) = edges.get_mut(from) {
        if let Some(k) = v.iter().position(|x| x == e) {
            v.remove(k);
        }
    }
}

impl<'a> Resolver<'a> {
    /// The text of the argument list of the call at (`line`, `site.col`) in module `gm`.
    fn args_at(&self, gm: &crate::model::Module, line: usize, col: i64, extra: usize) -> Vec<String> {
        let hi = (line + extra).min(gm.code.len());
        let code = gm.code[(line - 1).min(hi)..hi].join("\n");
        let from = code.char_indices().nth(col.max(0) as usize).map(|x| x.0).unwrap_or(code.len());
        let o = code[from..].find('(').map(|x| x + from);
        let close = o.map(|o| matching(&code, o)).unwrap_or(-1);
        if close > 0 {
            split_top(&code[o.unwrap() + 1..close as usize], ',')
        } else {
            vec![]
        }
    }

    /// The one project function every caller of f passes as its k-th argument
    /// (its id), or None.
    fn passed(&self, redges: &Edges, f: &'a Func, k: usize, depth: usize) -> Option<String> {
        let g = self.g;
        let callers = redges.get(&f.id).cloned().unwrap_or_default();
        if depth > 4 || callers.is_empty() || !callers.iter().all(|e| e.2) {
            return None;
        }
        let mut got: Vec<String> = Vec::new();
        let mut pn = self.param_names(f);
        while pn.len() <= k {
            pn.push(String::new());
        }
        let pname = pn[k].clone();
        for (src, line, _) in &callers {
            let gf = g.func(src);
            let gm = gf.and_then(|gf| g.by_file.get(&gf.file)?.iter().map(|n| &g.modules[n]).find(|m| m.name == gf.module));
            let site = gf.and_then(|gf| gf.calls.iter().find(|c2| c2.line == *line && c2.name == f.name && c2.col >= 0));
            let (Some(gf), Some(gm), Some(site)) = (gf, gm, site) else { return None };
            let args = self.args_at(gm, *line, site.col, 30);
            let arg = args.get(k).map(|a| a.trim().to_string()).unwrap_or_default();
            if !re!(r"^[A-Za-z_$][\w$]*$").is_match(&arg) {
                return None;
            }
            if gf.id == f.id && arg == pname {
                continue; // a recursive call handing the parameter on
            }
            let gparams = self.param_names(gf);
            let hit_id = if gparams.contains(&arg) && !gf.bound.contains_key(&arg) {
                // the caller's own parameter: what its callers pass
                self.passed(redges, gf, gparams.iter().position(|x| *x == arg).unwrap(), depth + 1)
            } else {
                self.t.bare(gm, Some(gf), &arg).filter(|h| h.kind != "top").map(|h| h.id.clone())
            };
            let hit_id = hit_id?;
            if !got.contains(&hit_id) {
                got.push(hit_id);
            }
            if got.len() > 1 {
                return None;
            }
        }
        if got.len() == 1 {
            got.pop()
        } else {
            None
        }
    }

    /// The value every caller of f passes as its k-th argument (JS / TS): each
    /// caller exact, each argument a chain that evaluates to the same project
    /// type (or every one a library's value: a literal, a builtin's), or the
    /// caller's own parameter whose callers do; else None.
    fn arg_value(&self, redges: &Edges, f: &'a Func, k: usize, depth: usize) -> Option<Val> {
        let g = self.g;
        let callers = redges.get(&f.id).cloned().unwrap_or_default();
        if depth > 3 || callers.is_empty() || !callers.iter().all(|e| e.2) || callers.len() > 40 {
            return None;
        }
        let mut got: Vec<Option<ArgVal>> = Vec::new();
        let add = |got: &mut Vec<Option<ArgVal>>, v: Option<ArgVal>| {
            if !got.contains(&v) {
                got.push(v);
            }
        };
        for (src, line, _) in &callers {
            let gf = g.func(src);
            let gm = gf.and_then(|gf| g.modules.get(&gf.module));
            let site = gf.and_then(|gf| gf.calls.iter().find(|c2| c2.line == *line && c2.name == f.name && c2.col >= 0));
            let (Some(gf), Some(gm), Some(site)) = (gf, gm, site) else { return None };
            if !(0 < *line && *line <= gm.code.len()) {
                return None;
            }
            let args = self.args_at(gm, *line, site.col, 30);
            if args.len() <= k || args[..=k].iter().any(|a| a.trim().starts_with("...")) {
                return None;
            }
            let text = args[k].trim().to_string();
            if re!(r#"(?s)^(?:'(?:[^'\\\n]|\\.)*'|"(?:[^"\\\n]|\\.)*"|`[^`$]*`|-?\d[\d_.]*|\[.*\])$"#).is_match(&text) {
                add(&mut got, Some(ArgVal::Ext)); // a string, a number, an array: the runtime's
                if got.len() > 1 {
                    return None;
                }
                continue;
            }
            let gparams = self.param_names(gf);
            if re!(r"^[A-Za-z_$][\w$]*$").is_match(&text) && gparams.contains(&text) && !gf.bound.contains_key(&text) {
                let v = self.arg_value(redges, gf, gparams.iter().position(|x| *x == text).unwrap(), depth + 1); // handed on from its own callers
                let a = v.map(|v| if v.is_ext() { ArgVal::Ext } else { let mut m = v.mods().to_vec(); m.sort(); ArgVal::Inst(m) });
                add(&mut got, a);
                if got.contains(&None) || got.len() > 1 {
                    return None;
                }
                continue;
            }
            let ch = expression(&text, 0, 40, Default::default());
            let mut flat = text.clone();
            while re!(r"\([^()]*[^()\s][^()]*\)").search(&flat).is_some() {
                flat = re!(r"\([^()]*\)").sub("()", &flat); // (the arguments: not part of the chain)
            }
            let strip = |s: &str| re!(r"\s+|\bnew\b|\??\.").sub("", s);
            if ch.is_empty() || strip(&flat) != strip(&ch.join("")) {
                return None; // (not a plain chain: `a || b`, a literal)
            }
            self.t.line.set(*line);
            let v = self.t.chain(gm, Some(gf), &ch, 0, None, None);
            let v = match v {
                None | Some(Val::Unknown) => return None,
                Some(v) => v,
            };
            let a = if v.is_ext() {
                Some(ArgVal::Ext)
            } else if let Val::Inst(m, true) = &v {
                let mut m = m.clone();
                m.sort();
                Some(ArgVal::Inst(m))
            } else {
                None
            };
            add(&mut got, a);
            if got.contains(&None) || got.len() > 1 {
                return None;
            }
        }
        match got.pop()?? {
            ArgVal::Ext => Some(crate::graph::infer::ext()),
            ArgVal::Inst(m) => Some(Val::Inst(m, true)),
        }
    }

    /// A JS / TS call guessed on a parameter (`headers.set(k, v)` in
    /// `setFormDataHeaders(headers, ...)`): typed by what every caller passes,
    /// the call reaches that type's method, or (a library value) nothing.
    pub(super) fn param_receivers(&self, edges: &mut Edges, redges: &mut Edges, pending: &[(String, Call)]) {
        let g = self.g;
        for (fid, c) in pending {
            let f = g.func(fid).unwrap();
            let params = self.param_names(f);
            let Some(pos) = params.iter().position(|p| *p == c.chain[0]) else { continue };
            let Some(v) = self.arg_value(redges, f, pos, 0) else { continue };
            let md = &g.modules[&f.module];
            self.t.line.set(c.line);
            let v = if c.chain.len() > 1 { self.t.chain(md, Some(f), &c.chain, 0, None, Some(v)) } else { Some(v) };
            let v = match v {
                None | Some(Val::Unknown) => continue,
                Some(v) => v,
            };
            let hits: Vec<&Func> = if v.is_ext() {
                vec![]
            } else if let Val::Inst(mods, _) = &v {
                let h = self.t.method(mods, &c.name, None).0;
                if h.is_empty() {
                    continue;
                }
                h
            } else {
                continue;
            };
            let enclosing: Vec<String> = g
                .funcs
                .keys()
                .filter_map(|id| g.func(id))
                .filter(|h| h.file == f.file && h.start <= f.start && f.end <= h.end)
                .map(|h| h.id.clone())
                .collect();
            for eid in enclosing {
                let old: Vec<Edge> = edges
                    .get(&eid)
                    .map(|v| v.iter().filter(|(d, ln, e)| *ln == c.line && !*e && d.rsplit('.').next() == Some(c.name.as_str())).cloned().collect())
                    .unwrap_or_default();
                if old.is_empty() {
                    continue;
                }
                for (d, ln, e) in old {
                    remove_edge(edges, &eid, &(d.clone(), ln, e));
                    remove_edge(redges, &d, &(eid.clone(), ln, e));
                }
                for h in &hits {
                    edges.entry(eid.clone()).or_default().push((h.id.clone(), c.line, true));
                    redges.entry(h.id.clone()).or_default().push((eid.clone(), c.line, true));
                }
            }
        }
    }

    /// A call through a parameter (`checkFn(x)` in `runCheck(checkFn)`): when
    /// every caller is exact and passes the same project function there by name
    /// (or hands on its own parameter, whose callers do), the call reaches that
    /// function.
    pub(super) fn param_flow(&self, edges: &mut Edges, redges: &mut Edges, pending: &[(String, Call)]) {
        let g = self.g;
        for (fid, c) in pending {
            let f = g.func(fid).unwrap();
            let params = self.param_names(f);
            let Some(pos) = params.iter().position(|p| *p == c.name) else { continue };
            let Some(got) = self.passed(redges, f, pos, 0) else { continue };
            let target = &g.funcs[&got];
            let target = &g.modules[&target.0].funcs[target.1];
            let old: Vec<Edge> = edges
                .get(&f.id)
                .map(|v| v.iter().filter(|(d, ln, e)| *ln == c.line && d.rsplit('.').next() == Some(c.name.as_str()) && !*e).cloned().collect())
                .unwrap_or_default();
            for (d, ln, e) in old {
                remove_edge(edges, &f.id, &(d.clone(), ln, e));
                remove_edge(redges, &d, &(f.id.clone(), ln, e));
            }
            edges.entry(f.id.clone()).or_default().push((target.id.clone(), c.line, true));
            redges.entry(target.id.clone()).or_default().push((f.id.clone(), c.line, true));
        }
    }

    pub(super) fn _unused(&self) -> bool {
        ends("", &[])
    }
}
