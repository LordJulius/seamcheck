//! The lints that walk the call graph: `auth-in-background`
//! (lint_infra.auth_in_background) and `ets-owner` (lint.ets_owner).

use serde_json::{Map, Value};

use super::{balanced_end, line, opt_int, opt_str, Hits};
use crate::graph::infer::re;
use crate::graph::Graph;
use crate::model::Func;
use crate::project::Project;
use crate::pyre::Rx;
use crate::rules::{compile, RuleErr};

const AUTH_NAME: &str =
    r"^(?:login|log_in|sign_in|signin|authenticate|reauth\w*|reconnect\w*|relogin|refresh_login)$";

/// A timer / handle_info / handle_continue that reaches a login or reauth
/// (an approval prompt at a random time).
pub fn auth_in_background(rel: &str, _raw: &str, _code: &str, o: &Map<String, Value>, p: &Project) -> Result<Hits, RuleErr> {
    if !rel.ends_with(".ex") {
        return Ok(vec![]);
    }
    let auth = compile(&opt_str(o, "auth", AUTH_NAME))?;
    let roots = compile(&opt_str(o, "roots", r"^(?:handle_info|handle_continue|init)$"))?;
    let depth = opt_int(o, "depth", 3).max(0) as usize;
    let g = p.graph();
    let mut out = Vec::new();
    for mn in g.by_file.get(rel).map(|v| v.as_slice()).unwrap_or(&[]) {
        for f in &g.modules[mn].funcs {
            if roots.match_start(&f.name).is_none() {
                continue;
            }
            let mut bad: Vec<String> = g
                .reach(&f.id, depth, false, true)
                .into_iter()
                .filter(|(fid, d)| *d > 0 && g.func(fid).map(|h| auth.match_start(&h.name).is_some()).unwrap_or(false))
                .map(|(fid, _)| fid)
                .collect();
            bad.sort();
            if let Some(b) = bad.first() {
                out.push((
                    f.start,
                    format!(
                        "{} reaches {b}: a timer or message can start an auth/login without the user asking (an \
                         approval prompt at a random time). Renew silently (refresh token) and leave login to an \
                         explicit user action.",
                        f.name
                    ),
                ));
            }
        }
    }
    Ok(out)
}

fn process() -> &'static Rx {
    re!(r"(?m)^\s*use\s+(?:GenServer|Supervisor|DynamicSupervisor|Agent|GenStage|Application|Oban\.Worker)\b|@behaviou?r\s+(?:GenServer|:gen_\w+|Supervisor|Application)\b|:start_link,\s*\[[^\]]*__MODULE__|\.start_link\(\s*(?:\{[^}]*\},\s*)?__MODULE__")
}

/// A process callback of a module that is a process: init / handle_*, or any
/// `@impl` callback (`init_cycle` of a :gen_cycle, a :gen_statem state).
fn is_owner(p: &Project, g: &Graph, h: &Func, owners: &Rx) -> bool {
    let imp = g
        .module(&h.module)
        .map(|m| {
            (h.start.saturating_sub(3)..h.start.saturating_sub(1))
                .any(|i| i < m.raw.len() && re!(r"\s*@impl\b").match_start(&m.raw[i]).is_some())
        })
        .unwrap_or(false);
    if !(owners.match_start(&h.name).is_some() || imp) {
        return false;
    }
    process().is_match(&p.text(&h.file))
}

/// (some path into f starts at a process callback, a product caller that isn't one).
fn owner_path(p: &Project, g: &Graph, f: &Func, owners: &Rx, depth: usize, tests: &Rx) -> (bool, String) {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::from([f.id.clone()]);
    let mut frontier: Vec<&Func> = vec![f];
    let mut culprit = String::new();
    for _ in 0..depth {
        let mut nxt = Vec::new();
        for h0 in &frontier {
            for (c, _, ex) in g.redges.get(&h0.id).map(|v| v.as_slice()).unwrap_or(&[]) {
                let Some(h) = g.func(c) else { continue };
                if !ex || seen.contains(c) || tests.is_match(&h.file) {
                    continue;
                }
                seen.insert(c.clone());
                if is_owner(p, g, h, owners) || (h.kind == "top" && h.file.ends_with(".exs")) {
                    return (true, String::new()); // (a boot script, test_helper.exs: its process lives the whole run)
                }
                if culprit.is_empty() && (h.module != f.module || h.kind != "private") {
                    culprit = format!("{}.{}", h.module, h.name);
                }
                nxt.push(h);
            }
        }
        frontier = nxt;
    }
    (false, culprit)
}

/// An ETS table created outside a long-lived owner dies with whichever
/// process made it.
pub fn ets_owner(rel: &str, _raw: &str, code: &str, o: &Map<String, Value>, p: &Project) -> Result<Hits, RuleErr> {
    if super::ui::lang(rel) != "elixir" {
        return Ok(vec![]);
    }
    let owners = compile(&opt_str(o, "owners", r"^(init|start|handle_\w+|terminate|code_change)$"))?;
    let tests = compile(&opt_str(o, "tests", r"(^|/)test/|_test\.exs$"))?;
    let depth = opt_int(o, "depth", 5).max(0) as usize;
    let g = p.graph();
    let mods: Vec<&crate::model::Module> =
        g.by_file.get(rel).map(|v| v.iter().map(|mn| &g.modules[mn]).collect()).unwrap_or_default();
    let mut out = Vec::new();
    for m in re!(r":ets\.new\(").find_iter(code) {
        let ln = line(code, m.start());
        let args = super::sl(code, m.end(), balanced_end(code, m.end(), ""));
        if args.contains("heir") {
            continue;
        }
        let mut f: Option<&Func> = None;
        for md in &mods {
            if let Some(h) = md.func_at(ln) {
                f = Some(h);
            }
        }
        let Some(f) = f else { continue };
        if is_owner(p, &g, f, &owners) {
            continue;
        }
        let (owned, culprit) = owner_path(p, &g, f, &owners, depth, &tests);
        if owned {
            continue;
        }
        let callers: Vec<Option<&Func>> =
            g.redges.get(&f.id).map(|v| v.as_slice()).unwrap_or(&[]).iter().filter(|e| e.2).map(|e| g.func(&e.0)).collect();
        if culprit.is_empty() && !callers.is_empty() && callers.iter().all(|h| h.map(|h| tests.is_match(&h.file)).unwrap_or(false)) {
            continue; // only test setup makes it (test_helper.exs): the test run's process owns it
        }
        let who = if culprit.is_empty() {
            "no process's init or handle_* reaches it".to_string()
        } else {
            format!("reached from {culprit}")
        };
        out.push((
            ln,
            format!(
                "ETS table created in {} ({who}): a table dies with the process that made it, so whichever request \
                 comes first owns the cache and takes it down when it ends (PMTiles: \"the table identifier does not \
                 refer to an existing ETS table\"). Create it in a supervised owner's init (e.g. a small GenServer \
                 under the application) or pass :heir.",
                f.name
            ),
        ));
    }
    Ok(out)
}
