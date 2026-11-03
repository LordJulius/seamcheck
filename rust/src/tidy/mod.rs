//! Keeping a big repo tidy (tidy.py): what's dead, what's duplicated, which
//! tests test nothing, where a god module splits. Each report is
//! conservative: a function counts as unused only if nothing calls it AND its
//! name appears in no other file (scripts, config and templates included),
//! because frameworks, routers and `mix run -e` call things no call graph sees.
//!
//! Configured by the project's `[tidy]` table: `tests`, `entry`, `entry_in`,
//! `corpus`, `suites`, `domains` (see tidy.py).

pub mod audit;
pub mod cli;
pub mod corpus;
pub mod dead;
pub mod dups;
pub mod exactness;
pub mod remove;
pub mod split;

use std::collections::{BTreeSet, HashMap};

use rayon::prelude::*;
use serde_json::{Map, Value};

use crate::config::list_or;
use crate::glob::Globs;
use crate::graph::Graph;
use crate::model::{Func, Module};
use crate::project::Project;
use crate::pyre::Rx;

pub use corpus::Corpus;

/// Names a framework or runtime calls: never "unused" because nothing in the
/// graph calls them (tidy.ENTRY).
fn entry_rx() -> &'static Rx {
    static R: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        Rx::lit(concat!(
            r"^(main|init|start|stop|start_link|child_spec|terminate|code_change|format_status|handle_\w+|mount|render|",
            r"call|run|perform|execute|update|changeset|index|show|create|delete|new|edit|join|connect|id|fetch|",
            r"get_and_update|pop|cast|dump|load|embed_as|equal\?|type|spec|definition|schema|describe|validate\w*|",
            r"tool_definition|primitive_specs?|setup\w*|teardown\w*|before\w*|after\w*|on[A-Z]\w*|_\w+|__\w+__|",
            r"test\w*|constructor|toString|equals|hashCode|onCreate|onStart|onResume|onPause|onStop|onDestroy|",
            r"invoke|apply|default|render\w*|use[A-Z]\w*|get[A-Z]\w*Props)$"
        ))
    })
}

fn annotation_rx() -> &'static Rx {
    static R: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
    // a framework's registration on any object (a Flask blueprint's `@bp.route`,
    // `@socketio.on`, `@celery.task`, `@login.user_loader`) too
    R.get_or_init(|| {
        Rx::lit(concat!(
            r"\s*@(impl|Override|Test|Preview|app\.|router\.|route|get|post|put|delete|patch|shared_task|",
            r"[A-Za-z_]\w*\.(?:route|get|post|put|delete|patch|websocket|on|on_event|task|user_loader|",
            r"request_loader|errorhandler|app_errorhandler|before_request|before_app_request|after_request|",
            r"teardown_request|context_processor|cli\.command|command))\b"
        ))
    })
}

/// A loaded project and its graph, with what every report asks again and
/// again (is this a test file?) worked out once.
pub struct Tidy<'a> {
    pub p: &'a Project,
    pub g: &'a Graph,
    tests: HashMap<String, bool>,
    test_globs: Option<Globs>,
    entry: Option<Rx>,
    entry_in: Globs,
}

impl<'a> Tidy<'a> {
    pub fn new(p: &'a Project, g: &'a Graph) -> Tidy<'a> {
        let c = cfg(p);
        let test_globs = c.get("tests").map(|_| Globs::new(&list_or(c, "tests", &[])));
        let entry = c.get("entry").map(crate::config::py_str).filter(|s| !s.is_empty()).and_then(|s| Rx::new(&s).ok());
        let entry_in = Globs::new(&list_or(c, "entry_in", &[]));
        let mut t = Tidy { p, g, tests: HashMap::new(), test_globs, entry, entry_in };
        let files: BTreeSet<&String> = p.files.iter().chain(g.by_file.keys()).collect();
        let tests: Vec<(String, bool)> = files.into_par_iter().map(|r| (r.clone(), t.test_of(r))).collect();
        t.tests = tests.into_iter().collect();
        t
    }

    fn test_of(&self, rel: &str) -> bool {
        match &self.test_globs {
            Some(g) => g.any(rel),
            None => crate::gate::is_test(self.p, rel),
        }
    }

    /// tidy.is_test
    pub fn is_test(&self, rel: &str) -> bool {
        self.tests.get(rel).copied().unwrap_or_else(|| self.test_of(rel))
    }

    pub fn module(&self, f: &Func) -> Option<&'a Module> {
        self.g.modules.get(&f.module)
    }

    /// Functions in load order (project.funcs.values()).
    pub fn funcs(&self) -> impl Iterator<Item = &'a Func> + 'a {
        let g = self.g;
        g.funcs.keys().filter_map(move |id| g.func(id))
    }

    /// tidy.is_entry: a name frameworks call, a file whose functions are all
    /// entry points, or an annotation right above (@impl, @Override, a route).
    pub fn is_entry(&self, f: &Func) -> bool {
        if entry_rx().match_start(&f.name).is_some() || self.entry.as_ref().is_some_and(|e| e.match_start(&f.name).is_some())
        {
            return true;
        }
        if self.entry_in.any(&f.file) {
            return true;
        }
        let raw: &[String] = self.module(f).map(|m| m.raw.as_slice()).unwrap_or(&[]);
        let lo = f.start.saturating_sub(4);
        let hi = f.start.saturating_sub(1);
        (lo..hi).any(|i| raw.get(i).is_some_and(|l| annotation_rx().match_start(l).is_some()))
    }

    /// `name` and the lines of a function's source (`mod.raw[start-1:end]`).
    pub fn raw_lines(&self, f: &Func) -> &'a [String] {
        let Some(m) = self.module(f) else { return &[] };
        slice(&m.raw, f.start.saturating_sub(1), f.end)
    }
}

/// Python's `xs[a:b]` on a slice (clamped).
pub fn slice<T>(xs: &[T], a: usize, b: usize) -> &[T] {
    let b = b.min(xs.len());
    let a = a.min(b);
    &xs[a..b]
}

pub fn cfg(p: &Project) -> &Map<String, Value> {
    p.config.table("tidy")
}

/// Files changed since `ref` (committed, staged, unstaged) plus untracked (tidy.changed_files).
pub fn changed_files(p: &Project, r: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for args in [vec!["diff", "--name-only", r], vec!["ls-files", "--others", "--exclude-standard"]] {
        if let Some(o) = crate::gate::git(&p.root, &args) {
            out.extend(o.lines().map(|l| crate::pyre::strip(l)).filter(|l| !l.is_empty()).map(String::from));
        }
    }
    out
}

/// os.path.normpath for the relative paths tests name.
pub fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let lead = if path.starts_with("//") && !path.starts_with("///") {
        "//"
    } else if path.starts_with('/') {
        "/"
    } else {
        ""
    };
    let mut parts: Vec<&str> = Vec::new();
    for c in path.split('/') {
        if c.is_empty() || c == "." {
            continue;
        }
        if c != ".." || (lead.is_empty() && parts.is_empty()) || parts.last() == Some(&"..") {
            parts.push(c);
        } else if !parts.is_empty() {
            parts.pop();
        }
    }
    let s = format!("{lead}{}", parts.join("/"));
    if s.is_empty() {
        ".".into()
    } else {
        s
    }
}

/// os.path.dirname
pub fn dirname(rel: &str) -> &str {
    match rel.rfind('/') {
        Some(k) => {
            let d = &rel[..k];
            let t = d.trim_end_matches('/');
            if t.is_empty() {
                &rel[..1]
            } else {
                t
            }
        }
        None => "",
    }
}

/// os.path.join(a, b) for relative parts.
pub fn join(a: &str, b: &str) -> String {
    if b.starts_with('/') || a.is_empty() {
        b.to_string()
    } else if a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

/// Everywhere `name` is called (graph) or mentioned (any indexed file) (tidy.refs).
pub fn refs(t: &Tidy, name: &str, corpus: &Corpus) -> Value {
    let g = t.g;
    let fs: Vec<&Func> =
        t.funcs().filter(|f| f.name == name || f.id == name || f.id.ends_with(&format!(".{name}"))).collect();
    let mods: Vec<&Module> = g
        .modules
        .values()
        .filter(|m| m.name == name || m.name.ends_with(&format!(".{name}")) || m.name.ends_with(&format!("/{name}")))
        .collect();
    let guess = |ex: bool| if ex { "" } else { " (name guess)" };
    if !mods.is_empty() && fs.is_empty() {
        let ids: std::collections::HashSet<&str> = mods.iter().flat_map(|m| m.funcs.iter().map(|f| f.id.as_str())).collect();
        let mut callers: BTreeSet<(String, bool)> = BTreeSet::new();
        for i in &ids {
            for (c, _, ex) in g.redges.get(*i).map(|v| v.as_slice()).unwrap_or(&[]) {
                if !ids.contains(c.as_str()) {
                    callers.insert((c.clone(), *ex));
                }
            }
        }
        let short = name.rsplit('.').next().unwrap_or(name);
        let short = short.rsplit('/').next().unwrap_or(short);
        let mentions = corpus.mentions(short, &[short]);
        return serde_json::json!({
            "defined": mods.iter().map(|m| format!("{}  {}", m.name, m.file)).collect::<Vec<_>>(),
            "callers": callers.iter().map(|(c, ex)| format!("{c}{}", guess(*ex))).collect::<Vec<_>>(),
            "mentions": mentions.iter().map(|(r, n)| format!("{r} x{n}")).collect::<Vec<_>>(),
        });
    }
    let short = match fs.first() {
        Some(f) => f.name.clone(),
        None => name.rsplit('.').next().unwrap_or(name).to_string(),
    };
    let mut callers: BTreeSet<(String, bool)> = BTreeSet::new();
    for f in &fs {
        for (c, _, ex) in g.redges.get(&f.id).map(|v| v.as_slice()).unwrap_or(&[]) {
            callers.insert((c.clone(), *ex));
        }
    }
    let (bang, ask) = (format!("{short}!"), format!("{short}?"));
    let mentions = corpus.mentions(&short, &[short.as_str(), bang.as_str(), ask.as_str()]);
    serde_json::json!({
        "defined": fs.iter().map(|f| format!("{}  {}:{}", f.id, f.file, f.start)).collect::<Vec<_>>(),
        "callers": callers.iter().map(|(c, ex)| format!("{c}{}", guess(*ex))).collect::<Vec<_>>(),
        "mentions": mentions.iter().map(|(r, n)| format!("{r} x{n}")).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normpath_like_python() {
        assert_eq!(normpath("./x/../y/./z"), "y/z");
        assert_eq!(normpath("../../scripts/x.py"), "../../scripts/x.py");
        assert_eq!(normpath("test/deploy/../../scripts/x.py"), "scripts/x.py");
        assert_eq!(normpath("a/../../b"), "../b");
        assert_eq!(normpath(""), ".");
        assert_eq!(normpath("/a/../.."), "/");
        assert_eq!(dirname("test/a_test.exs"), "test");
        assert_eq!(dirname("x.exs"), "");
    }
}
