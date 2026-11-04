//! Precise mode (precise.py): a language server settles the calls the
//! static resolver can't.
//!
//! seamcheck's own resolver reads names. A call on a receiver whose type it
//! can't infer is a name guess (every function of that name), and some calls
//! reach nothing though a project function has the name. With `--precise`
//! (or `[precise] enabled = true`) those calls go to the language's server
//! as definition requests: an answer inside the project makes the call one
//! exact edge; an answer outside it (the standard library, a dependency)
//! makes the guesses nothing. A call the server can't answer stays as it
//! was, so precise mode only ever replaces a guess with what the server knows.
//!
//! Any LSP server over stdio works, per file extension (`[precise] servers`).
//! Built in, used when found: pyright (.py), rust-analyzer (.rs), gopls (.go)
//! and TypeScript's tsserver (.ts/.tsx/.js..., its own protocol, not LSP).
//! Answers are cached by the asking file's content, so a warm run asks
//! nothing (in a JSON file of its own beside Python's pickle). A missing or
//! crashing server is a warning on stderr, never a failed run.

pub mod lsp;
mod servers;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use indexmap::IndexMap;
use serde_json::{json, Value};

use crate::graph::link::{named, walk_order};
use crate::graph::{Graph, TraceRow};
use crate::model::{Call, Func};
use crate::project::{ext_of, Project};
use crate::pyre::Rx;
use lsp::{Batch, Client, Dead};
pub use servers::{servers, which};

const WAIT: f64 = 300.0; // seconds a server may take to load the workspace before its first answer
const TIMEOUT: f64 = 60.0; // seconds without any answer before a busy server is given up on

/// What the servers settled (`stats --precise`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub asked: usize,
    pub cached: usize,
    pub exact_from_guess: usize,
    pub exact_from_nothing: usize,
    pub library: usize,
    pub confirmed: usize,
    pub unsettled: usize,
    /// server name -> (asked, answered, seconds)
    pub servers: BTreeMap<String, (usize, usize, f64)>,
    pub warnings: Vec<String>,
    pub ms: u128,
}

impl Stats {
    /// As Python's `project.precise` dict prints in `stats --json`.
    pub fn to_json(&self) -> Value {
        let srv: serde_json::Map<String, Value> =
            self.servers.iter().map(|(k, (n, a, s))| (k.clone(), json!([n, a, s]))).collect();
        json!({"asked": self.asked, "cached": self.cached, "exact_from_guess": self.exact_from_guess,
               "exact_from_nothing": self.exact_from_nothing, "library": self.library, "confirmed": self.confirmed,
               "unsettled": self.unsettled, "servers": srv, "warnings": self.warnings, "ms": self.ms})
    }
}

/// `[precise] enabled` in the config.
pub fn enabled(p: &Project) -> bool {
    crate::config::truthy(p.config.table("precise").get("enabled"))
}

/// `stats --precise`: what the servers settled.
pub fn summary(p: &Stats) -> String {
    let srv: Vec<String> = p
        .servers
        .iter()
        .map(|(k, (n, a, s))| format!("{k}: {n} asked, {a} answered, {} s", crate::runtime::pyfmt::float_py(*s)))
        .collect();
    let srv = srv.join("; ");
    let out = format!(
        "precise: {} calls asked ({} answered from cache): {} guesses and {} nothings made exact, {} guesses were library calls, {} nothings confirmed, {} unsettled  ({} ms{})",
        p.asked,
        p.cached,
        p.exact_from_guess,
        p.exact_from_nothing,
        p.library,
        p.confirmed,
        p.unsettled,
        p.ms,
        if srv.is_empty() { String::new() } else { format!("; {srv}") }
    );
    std::iter::once(out).chain(p.warnings.iter().map(|w| format!("  {w}"))).collect::<Vec<_>>().join("\n")
}

// --- positions ----------------------------------------------------------------------

/// A character column as LSP counts it (UTF-16 code units).
pub fn to_u16(line: &str, col: usize) -> usize {
    if line.is_ascii() {
        col
    } else {
        line.chars().take(col).map(char::len_utf16).sum()
    }
}

pub fn from_u16(line: &str, u: usize) -> usize {
    if line.is_ascii() {
        return u;
    }
    let mut n = 0;
    for (i, ch) in line.chars().enumerate() {
        if n >= u {
            return i;
        }
        n += ch.len_utf16();
    }
    line.chars().count()
}

/// Where the called name starts on its line (a character column): the
/// adapter's column when it holds the name, else the name's one occurrence
/// (None: none or several).
pub fn column(text: &str, name: &str, col: i64) -> Option<usize> {
    let cs: Vec<char> = text.chars().collect();
    let nc: Vec<char> = name.chars().collect();
    if col >= 0 {
        let c = col as usize;
        if cs.get(c..c + nc.len()) == Some(&nc[..]) {
            return Some(c);
        }
    }
    let rx = Rx::new(&format!(r"(?<![\w$]){}(?![\w$])", regex_escape(name))).ok()?;
    let hits = rx.find_iter(text);
    match hits.as_slice() {
        [one] => Some(text[..one.start()].chars().count()),
        _ => None,
    }
}

fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if !(c.is_alphanumeric() || c == '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// --- settling the calls --------------------------------------------------------------

/// (func, call, outcome, callees) per call, from the trace the linker wrote
/// in the same order it walked the calls.
fn outcomes<'g>(g: &'g Graph, trace: &[TraceRow]) -> Vec<(&'g Func, &'g Call, &'static str, Vec<String>)> {
    let mut out = Vec::new();
    let mut i = 0;
    for fid in walk_order(g) {
        let Some(f) = g.func(&fid) else { continue };
        for c in &f.calls {
            if let Some(t) = trace.get(i) {
                if t.file == f.file && t.line == c.line && t.name == c.name && t.chain == c.chain {
                    out.push((f, c, t.outcome, t.ids.clone()));
                    i += 1;
                }
            }
        }
    }
    out
}

/// The project function defined at rel:line named ident: [func], [] for a
/// class with no constructor of its own, None when nothing there is a function.
fn target(g: &Graph, rel: &str, line: usize, ident: &str) -> Option<Vec<String>> {
    let mods: Vec<&crate::model::Module> = g.by_file.get(rel).into_iter().flatten().filter_map(|m| g.modules.get(m)).collect();
    let mut best: Option<&Func> = None;
    for h in mods.iter().flat_map(|m| m.funcs.iter()) {
        if h.start <= line && line <= h.end && h.name == ident && best.is_none_or(|b| h.start > b.start) {
            best = Some(h);
        }
    }
    if let Some(b) = best {
        return Some(vec![b.id.clone()]);
    }
    for m in &mods {
        // a class: `Foo()` / `new Foo()` builds one
        if (!m.owner.is_empty() || m.kind == "class") && (m.name.rsplit('.').next() == Some(ident) || m.decls.iter().any(|d| d == ident)) {
            for ctor in ["__init__", "constructor"] {
                if let Some(t) = g.lookup(&m.name, ctor) {
                    return Some(vec![t.id.clone()]);
                }
            }
            return Some(vec![]);
        }
    }
    None
}

/// What a server's answer makes of a call.
#[derive(Clone, Debug, PartialEq)]
enum Verdict {
    /// the project functions it names
    Funcs(Vec<String>),
    /// a project class with no constructor of its own
    Exact,
    /// all of it outside the project
    Lib,
}

/// None when some of the answer isn't a function seamcheck knows (a
/// variable, an interface's signature: stay static). An overload's signature
/// beside the function it declares is that function. An answer at no name
/// (tsserver points `f = () => {}` at its arrow) means the called name.
fn settle(g: &Graph, locs: &[(String, usize, String)], files: &HashSet<&str>, name: &str) -> Option<Verdict> {
    let (mut funcs, mut lib, mut klass, mut other) = (Vec::<String>::new(), false, false, Vec::new());
    for (rel, line, ident) in locs {
        if rel.is_empty() || !files.contains(rel.as_str()) {
            lib = true; // outside the root, or a file the project excludes (node_modules, .venv, target)
            continue;
        }
        let ident = if ident.is_empty() { name } else { ident.as_str() };
        let t = target(g, rel, *line, ident);
        match &t {
            None => other.push((rel.clone(), ident.to_string())),
            Some(v) if v.is_empty() => klass = true,
            Some(v) => funcs.extend(v.iter().cloned()),
        }
    }
    let known = |rel: &str, ident: &str| funcs.iter().filter_map(|id| g.func(id)).any(|h| h.file == rel && h.name == ident);
    if other.iter().any(|(rel, ident)| !known(rel, ident)) {
        return None;
    }
    if !funcs.is_empty() {
        Some(Verdict::Funcs(funcs))
    } else if klass {
        Some(Verdict::Exact)
    } else if lib {
        Some(Verdict::Lib)
    } else {
        None
    }
}

/// The name an answer points at, from the file it points into (each read once).
#[derive(Default)]
struct Words {
    cache: HashMap<String, Vec<String>>,
}

impl Words {
    fn word(&mut self, path: &str, line0: usize, u16: usize) -> String {
        let lines = self.cache.entry(path.to_string()).or_insert_with(|| {
            crate::project::read_text(Path::new(path)).map(|t| t.split('\n').map(String::from).collect()).unwrap_or_default()
        });
        let Some(line) = lines.get(line0) else { return String::new() };
        static WORD: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
        let rx = WORD.get_or_init(|| Rx::new(r"[A-Za-z_$][\w$]*").unwrap());
        let at = from_u16(line, u16);
        let byte = line.char_indices().nth(at).map(|(b, _)| b).unwrap_or(line.len());
        rx.match_at(line, byte).map(|m| m.group0().to_string()).unwrap_or_default()
    }
}

/// A call to ask about: its function, the call, its outcome, and its
/// position (line0, u16).
struct Ask<'g> {
    f: &'g Func,
    c: &'g Call,
    outcome: &'static str,
    pos: (usize, usize),
}

type Argv = Vec<String>;

/// Per server: the calls to ask about (guesses, and nothing-though-named),
/// a few calls the static resolver is sure of (to tell when it's ready), and
/// how many guesses each (caller, line, name) holds.
#[allow(clippy::type_complexity)]
fn questions<'g>(
    g: &'g Graph,
    trace: &[TraceRow],
    found: &IndexMap<String, Argv>,
    texts: &mut dyn FnMut(&str) -> Vec<String>,
) -> (IndexMap<Argv, Vec<Ask<'g>>>, HashMap<Argv, Vec<(String, (usize, usize))>>, HashMap<(String, usize, String), usize>) {
    let mut asks: IndexMap<Argv, Vec<Ask>> = IndexMap::new();
    let mut probes: HashMap<Argv, Vec<(String, (usize, usize))>> = HashMap::new();
    let mut guessed: HashMap<(String, usize, String), usize> = HashMap::new();
    for (f, c, outcome, targets) in outcomes(g, trace) {
        if outcome == "guess" {
            *guessed.entry((f.id.clone(), c.line, c.name.clone())).or_insert(0) += 1;
        }
        let Some(argv) = found.get(ext_of(&f.file)) else { continue };
        if !matches!(outcome, "guess" | "named" | "exact") {
            continue;
        }
        let pr = probes.entry(argv.clone()).or_default();
        if outcome == "exact" && (targets.len() != 1 || pr.len() >= 3 || pr.iter().any(|(r, _)| *r == f.file)) {
            continue;
        }
        let ls = texts(&f.file);
        let col = if c.line > 0 && c.line <= ls.len() { column(&ls[c.line - 1], &c.name, c.col) } else { None };
        let Some(col) = col else { continue };
        let pos = (c.line - 1, to_u16(&ls[c.line - 1], col));
        if outcome == "exact" {
            pr.push((f.file.clone(), pos));
        } else {
            asks.entry(argv.clone()).or_default().push(Ask { f, c, outcome, pos });
        }
    }
    (asks, probes, guessed)
}

// --- the answer cache ----------------------------------------------------------------

/// Where answers are kept between runs (fingerprint.CACHE_DIR).
pub fn cache_dir() -> std::path::PathBuf {
    super::cache::default_dir()
}

fn cache_path(dir: &Path, root: &Path) -> std::path::PathBuf {
    use sha1::{Digest, Sha1};
    let common = std::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::path::absolute(root).unwrap_or(root.to_path_buf()).to_string_lossy().into_owned());
    let key: String = Sha1::digest(format!("repo|{common}").as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    dir.join(format!("{}-precise.json", &key[..16]))
}

fn digest(path: &Path) -> Option<String> {
    use blake2::digest::{Update, VariableOutput};
    let data = std::fs::read(path).ok()?;
    let mut h = blake2::Blake2bVar::new(16).ok()?;
    h.update(&data);
    let mut out = [0u8; 16];
    h.finalize_variable(&mut out).ok()?;
    Some(out.iter().map(|b| format!("{b:02x}")).collect())
}

/// file -> digest -> "server|line0|u16" -> [(rel, line, word)]; two digests per
/// file (the main checkout's and a lane's).
type Cache = IndexMap<String, IndexMap<String, IndexMap<String, Vec<(String, usize, String)>>>>;

fn cache_load(path: &Path) -> Cache {
    let Ok(text) = std::fs::read_to_string(path) else { return Cache::new() };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return Cache::new() };
    if v.get("v").and_then(Value::as_i64) != Some(1) {
        return Cache::new();
    }
    serde_json::from_value(v["files"].clone()).unwrap_or_default()
}

fn cache_save(path: &Path, c: &Cache) {
    let Some(dir) = path.parent() else { return };
    let _ = std::fs::create_dir_all(dir);
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, json!({"v": 1, "files": c}).to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

// --- asking ---------------------------------------------------------------------------

type Key = (String, usize, usize, String); // (rel, line0, u16, server) or ("\0probe", ...)
type Locs = Vec<(String, usize, usize)>;

/// {key: locations} from one server, whether it was ready (so an empty
/// answer means "no definition", worth caching), and a warning if it
/// failed. Answers that came before a crash are kept.
fn ask_server(argv: &Argv, root: &str, probes: &Batch<Key>, files: &Batch<Key>, wait: f64, timeout: f64) -> (HashMap<Key, Option<Locs>>, bool, String) {
    let mut client = match servers::client(argv, root) {
        Ok(c) => c,
        Err(e) => return (HashMap::new(), false, format!("precise: {} won't start: {}", argv[0], lsp::os_error(&e, &argv[0]))),
    };
    let mut out = HashMap::new();
    let run = |client: &mut Client, out: &mut HashMap<Key, Option<Locs>>| -> Result<bool, Dead> {
        client.start(wait)?;
        let ok = lsp::ready(client, probes, wait)?;
        lsp::drive(client, files, timeout, wait, out)?;
        Ok(ok)
    };
    let res = run(&mut client, &mut out);
    client.stop();
    match res {
        Ok(ok) => (out, ok, String::new()),
        Err(Dead(e)) => (out, false, format!("precise: {} {e}", servers::name(argv))),
    }
}

/// Replace the static outcome of every guessed call (and of every call that
/// reached nothing though a project function has the name) with what the
/// language servers say. Fills `g.precise` for `stats`. `cache`: the
/// directory of the answers from earlier runs (None: ask everything).
pub fn refine(g: &mut Graph, p: &Project, cache_in: Option<&Path>) {
    let t0 = Instant::now();
    let cfg = p.config.table("precise");
    let num = |k: &str, d: f64| cfg.get(k).and_then(Value::as_f64).unwrap_or(d);
    let (wait, timeout) = (num("wait", WAIT), num("timeout", TIMEOUT));
    let (found, mut warnings) = servers(&p.config);
    let trace = g.trace.take().unwrap_or_default();
    let mut memo: HashMap<String, Vec<String>> = HashMap::new();
    let mut texts = |rel: &str| -> Vec<String> { memo.entry(rel.to_string()).or_insert_with(|| p.text(rel).split('\n').map(String::from).collect()).clone() };
    let gr: &Graph = g;
    let (asks, probes, guessed) = questions(gr, &trace, &found, &mut texts);
    let cpath = cache_in.map(|d| cache_path(d, &p.root));
    let mut cache = cpath.as_deref().map(cache_load).unwrap_or_default();
    let mut digests: HashMap<String, String> = HashMap::new();
    for a in asks.values().flatten() {
        if !digests.contains_key(&a.f.file) {
            if let Some(d) = digest(&p.path(&a.f.file)) {
                digests.insert(a.f.file.clone(), d);
            }
        }
    }
    let ck = |sk: &str, pos: (usize, usize)| format!("{sk}|{}|{}", pos.0, pos.1);
    let mut answers: HashMap<Key, Vec<(String, usize, String)>> = HashMap::new();
    let mut jobs: Vec<(Argv, Batch<Key>, Batch<Key>)> = Vec::new();
    let mut cached = 0;
    for (argv, todo) in &asks {
        let sk = argv.join(" ");
        let mut need: BTreeMap<String, std::collections::BTreeSet<(usize, usize)>> = BTreeMap::new();
        for a in todo {
            let hit = digests.get(&a.f.file).and_then(|d| cache.get(&a.f.file).and_then(|s| s.get(d))).and_then(|s| s.get(&ck(&sk, a.pos)));
            if let Some(hit) = hit {
                answers.insert((a.f.file.clone(), a.pos.0, a.pos.1, sk.clone()), hit.clone());
                cached += 1;
            } else if digests.contains_key(&a.f.file) {
                need.entry(a.f.file.clone()).or_default().insert(a.pos);
            }
        }
        if !need.is_empty() {
            let batch: Batch<Key> = need
                .iter()
                .map(|(rel, ps)| (rel.clone(), texts(rel).join("\n"), ps.iter().map(|&(l, u)| (l, u, (rel.clone(), l, u, sk.clone()))).collect()))
                .collect();
            let pr: Batch<Key> = probes
                .get(argv)
                .into_iter()
                .flatten()
                .map(|(rel, (l, u))| (rel.clone(), texts(rel).join("\n"), vec![(*l, *u, ("\0probe".to_string() + rel, *l, *u, String::new()))]))
                .collect();
            jobs.push((argv.clone(), pr, batch));
        }
    }

    // every server at once, one thread each (a repo in three languages waits
    // for the slowest server, not for the sum)
    let root = p.root.to_string_lossy().into_owned();
    let results: Vec<(Argv, HashMap<Key, Option<Locs>>, bool, String, f64, usize)> = std::thread::scope(|s| {
        let hs: Vec<_> = jobs
            .iter()
            .map(|(argv, pr, batch)| {
                let root = root.clone();
                s.spawn(move || {
                    let s0 = Instant::now();
                    let (got, ok, warn) = ask_server(argv, &root, pr, batch, wait, timeout);
                    (argv.clone(), got, ok, warn, s0.elapsed().as_secs_f64(), batch.iter().map(|b| b.2.len()).sum())
                })
            })
            .collect();
        hs.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let mut words = Words::default();
    let prefix = format!("{root}/");
    let mut per_server: BTreeMap<String, (usize, usize, f64)> = BTreeMap::new();
    for (argv, got, ready, warn, secs, n) in results {
        if !warn.is_empty() {
            warnings.push(warn);
        }
        let mut answered = 0;
        for (k, locs) in got {
            let Some(locs) = locs else { continue };
            if k.0.starts_with("\0probe") || (locs.is_empty() && !ready) {
                continue; // an error, or "nothing" from a server that may not have loaded yet
            }
            answered += 1;
            let norm: Vec<(String, usize, String)> = locs
                .iter()
                .map(|(path, line0, u)| (path.strip_prefix(&prefix).unwrap_or("").to_string(), line0 + 1, words.word(path, *line0, *u)))
                .collect();
            let slot = cache.entry(k.0.clone()).or_default();
            if let Some(d) = digests.get(&k.0) {
                slot.entry(d.clone()).or_default().insert(ck(&k.3, (k.1, k.2)), norm.clone());
            }
            while slot.len() > 2 {
                slot.shift_remove_index(0);
            }
            answers.insert(k, norm);
        }
        per_server.insert(servers::name(&argv), (n, answered, (secs * 10.0).round() / 10.0));
    }
    if let Some(cp) = cpath.as_deref().filter(|_| !jobs.is_empty()) {
        cache_save(cp, &cache);
    }

    let mut st = Stats { cached, servers: per_server, ..Default::default() };
    let files: HashSet<&str> = p.files.iter().map(|s| s.as_str()).collect();
    let mut groups: IndexMap<(String, usize, String), Vec<(String, Call, Option<Verdict>)>> = IndexMap::new();
    // (what to do, in Python's order: the settled nothings as met, then each
    // group of guesses: its edges dropped, its calls applied)
    let mut steps: Vec<Step> = Vec::new();
    for (argv, todo) in &asks {
        let sk = argv.join(" ");
        for a in todo {
            st.asked += 1;
            let verdict = answers.get(&(a.f.file.clone(), a.pos.0, a.pos.1, sk.clone())).filter(|l| !l.is_empty()).and_then(|l| settle(gr, l, &files, &a.c.name));
            if a.outcome == "guess" {
                groups.entry((a.f.id.clone(), a.c.line, a.c.name.clone())).or_default().push((a.f.id.clone(), a.c.clone(), verdict));
            } else if let Some(v) = verdict {
                steps.push(Step::Apply(a.f.id.clone(), a.c.clone(), a.outcome, v));
            }
        }
    }
    for (k, calls) in groups {
        // a guess's edges go only when every guess of that name on the line is settled
        if calls.len() == guessed.get(&k).copied().unwrap_or(0) && calls.iter().all(|c| c.2.is_some()) {
            steps.push(Step::Drop(k.0, k.1, k.2));
            steps.extend(calls.into_iter().map(|(f, c, v)| Step::Apply(f, c, "guess", v.unwrap())));
        }
    }
    drop(asks);
    for step in steps {
        match step {
            Step::Apply(fid, c, outcome, v) => apply(g, &fid, &c, outcome, v, &mut st),
            Step::Drop(fid, line, name) => drop_guesses(g, &fid, line, &name),
        }
    }
    st.unsettled = st.asked - (st.exact_from_guess + st.exact_from_nothing + st.library + st.confirmed);
    st.warnings = warnings;
    st.ms = t0.elapsed().as_millis();
    for w in &st.warnings {
        eprintln!("seamcheck: {w}");
    }
    g.precise = Some(st);
}

enum Step {
    Apply(String, Call, &'static str, Verdict),
    Drop(String, usize, String),
}

fn drop_guesses(g: &mut Graph, fid: &str, line: usize, name: &str) {
    let gone: Vec<crate::graph::Edge> = g
        .edges
        .get(fid)
        .map(|es| es.iter().filter(|e| e.1 == line && !e.2 && g.func(&e.0).is_some_and(|h| h.name == name)).cloned().collect())
        .unwrap_or_default();
    if gone.is_empty() {
        return;
    }
    if let Some(es) = g.edges.get_mut(fid) {
        es.retain(|e| !gone.contains(e));
    }
    let tids: HashSet<&String> = gone.iter().map(|e| &e.0).collect();
    for tid in tids {
        if let Some(rs) = g.redges.get_mut(tid) {
            rs.retain(|r| !(r.0 == fid && r.1 == line && !r.2));
        }
    }
}

/// Move one call from guess / nothing to what the server said, keeping the
/// resolution counts in step.
fn apply(g: &mut Graph, fid: &str, c: &Call, outcome: &str, verdict: Verdict, st: &mut Stats) {
    let Some(f) = g.func(fid).cloned() else { return };
    let named_ = named(g, &f.file, &c.name);
    let res = g.resolution.entry(ext_of(&f.file).to_string()).or_insert([0; 4]);
    if verdict == Verdict::Lib {
        if outcome == "guess" {
            res[1] = res[1].saturating_sub(1);
            res[2] += 1;
            res[3] += named_ as usize;
            st.library += 1;
        }
        st.confirmed += (outcome == "named") as usize; // (it was right to reach nothing)
        return;
    }
    if outcome == "guess" {
        res[1] = res[1].saturating_sub(1);
    } else {
        res[2] = res[2].saturating_sub(1);
        res[3] = res[3].saturating_sub(1);
    }
    res[0] += 1;
    if outcome == "guess" {
        st.exact_from_guess += 1;
    } else {
        st.exact_from_nothing += 1;
    }
    let targets = match verdict {
        Verdict::Funcs(v) => v,
        _ => vec![],
    };
    // like the static resolver: a subclass's override may run too
    let mut all = targets.clone();
    for t in &targets {
        let Some(tf) = g.func(t) else { continue };
        for sub in g.children.get(&tf.module).into_iter().flatten() {
            if let Some(o) = g.func(&format!("{sub}.{}", tf.name)) {
                all.push(o.id.clone());
            }
        }
    }
    for t in all {
        g.edges.entry(f.id.clone()).or_default().push((t.clone(), c.line, true));
        g.redges.entry(t).or_default().push((f.id.clone(), c.line, true));
    }
}
