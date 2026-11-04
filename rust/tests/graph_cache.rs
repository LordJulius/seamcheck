//! The linked graph kept between runs (graph/cache.rs). Python has no tests
//! of its own for core.load's graph cache (only the gate's exactness records
//! use fingerprint.CACHE_DIR); these pin what the cache promises: a graph
//! read back is the graph built, a changed file, config or flag never reads
//! an old one, checkouts of one repository share, only the newest few stay.

mod common;

use std::path::Path;
use std::process::Command;

use seamcheck::graph::{cache, Graph};
use tempfile::TempDir;

const FILES: &[(&str, &str)] = &[
    ("app.py", "from lib import helper\n\ndef main():\n    helper()\n    return 1\n"),
    ("lib.py", "def helper():\n    return 2\n\ndef unused_one():\n    return 3\n"),
    ("seamcheck.toml", "[project]\ninclude = [\"**/*.py\"]\n"),
];

fn project() -> (TempDir, seamcheck::project::Project) {
    let d = TempDir::new().unwrap();
    common::write(d.path(), FILES);
    let p = common::load(d.path());
    (d, p)
}

/// Everything about a graph two runs must agree on.
fn shape(g: &Graph) -> String {
    let mut out = String::new();
    for (k, m) in &g.modules {
        out += &format!("{k} {} {:?} {:?}\n", m.file, m.aliases, m.imported);
        for f in &m.funcs {
            out += &format!("  {} {}-{} {:?} {:?}\n", f.id, f.start, f.end, f.calls, f.bound);
        }
    }
    for (k, es) in &g.edges {
        out += &format!("{k} -> {es:?}\n");
    }
    for (k, es) in &g.redges {
        out += &format!("{k} <- {es:?}\n");
    }
    out += &format!("{:?} {:?}\n", g.resolution, g.problems);
    out
}

fn cache_files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("-graph-rs-"))
        .collect();
    v.sort();
    v
}

// a graph read back is the graph that was built
#[test]
fn a_graph_read_back_is_the_graph_built() {
    let (_d, p) = project();
    let cdir = TempDir::new().unwrap();
    let built = Graph::load(&p);
    let slot = cache::slot_in(&p, true, cdir.path().to_path_buf());
    assert!(slot.load().is_none(), "nothing cached yet");
    slot.save(&built);
    let again = cache::slot_in(&p, true, cdir.path().to_path_buf()).load().expect("cached");
    assert_eq!(shape(&built), shape(&again));
    assert!(again.funcs.contains_key("app.main"));
    assert_eq!(built.funcs.len(), again.funcs.len());
    assert_eq!(built.by_name.len(), again.by_name.len());
}

// a file's modules that hold the same lines still share them after the round trip
#[test]
fn modules_of_one_file_share_their_lines() {
    let d = TempDir::new().unwrap();
    common::write(
        d.path(),
        &[
            ("m.py", "class A:\n    def f(self):\n        return 1\n\nclass B:\n    def g(self):\n        return 2\n"),
            ("seamcheck.toml", "[project]\ninclude = [\"**/*.py\"]\n"),
        ],
    );
    let p = common::load(d.path());
    let cdir = TempDir::new().unwrap();
    let built = Graph::load(&p);
    let shared = built.modules.values().filter(|m| m.file == "m.py").count();
    assert!(shared >= 2, "a module per class and one for the file");
    cache::slot_in(&p, true, cdir.path().to_path_buf()).save(&built);
    let again = cache::slot_in(&p, true, cdir.path().to_path_buf()).load().unwrap();
    let arcs: Vec<_> = again.modules.values().filter(|m| m.file == "m.py").map(|m| std::sync::Arc::as_ptr(&m.raw)).collect();
    assert!(arcs.windows(2).all(|w| w[0] == w[1]), "one copy of the file's lines");
}

// a changed file never reads the old graph
#[test]
fn a_changed_file_is_a_new_key() {
    let (d, p) = project();
    let cdir = TempDir::new().unwrap();
    cache::slot_in(&p, true, cdir.path().to_path_buf()).save(&Graph::load(&p));
    std::fs::write(d.path().join("lib.py"), "def helper():\n    return 2\n\ndef other():\n    return 4\n").unwrap();
    let p2 = common::load(d.path());
    assert!(cache::slot_in(&p2, true, cdir.path().to_path_buf()).load().is_none());
    // and the same content, touched, is still the same key
    std::fs::write(d.path().join("lib.py"), FILES[1].1).unwrap();
    let p3 = common::load(d.path());
    assert!(cache::slot_in(&p3, true, cdir.path().to_path_buf()).load().is_some());
}

// a new file is a new key; so is the config, the flag for bridges, and a manifest the graph reads
#[test]
fn config_flags_and_file_lists_are_in_the_key() {
    let (d, p) = project();
    let cdir = TempDir::new().unwrap();
    cache::slot_in(&p, true, cdir.path().to_path_buf()).save(&Graph::load(&p));
    assert!(cache::slot_in(&p, false, cdir.path().to_path_buf()).load().is_none(), "bridges off is another graph");
    common::write(d.path(), &[("extra.py", "def e():\n    return 1\n")]);
    assert!(cache::slot_in(&common::load(d.path()), true, cdir.path().to_path_buf()).load().is_none());
    std::fs::remove_file(d.path().join("extra.py")).unwrap();
    assert!(cache::slot_in(&common::load(d.path()), true, cdir.path().to_path_buf()).load().is_some());
    std::fs::write(d.path().join("seamcheck.toml"), "[project]\ninclude = [\"**/*.py\"]\n\n[[rule]]\nid = \"x\"\nkind = \"forbid\"\npattern = \"zzz\"\n").unwrap();
    assert!(cache::slot_in(&common::load(d.path()), true, cdir.path().to_path_buf()).load().is_none(), "config");
}

// the command line: second run reads the cache, an edit is seen, --no-cache writes nothing
fn run(root: &Path, cdir: &Path, args: &[&str]) -> (String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_seamcheck"))
        .args(args)
        .current_dir(root)
        .env("SEAMCHECK_RS_QUIET", "1")
        .env("SEAMCHECK_CACHE_DIR", cdir)
        .env("SEAM_TIMING", "1")
        .output()
        .unwrap();
    (String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn the_command_line_reads_the_cache_and_sees_edits() {
    let (d, _p) = project();
    let cdir = TempDir::new().unwrap();
    let (out1, err1) = run(d.path(), cdir.path(), &["callers", "helper"]);
    assert!(!err1.contains("cache load"), "first run builds: {err1}");
    assert_eq!(cache_files(cdir.path()).len(), 1);
    let (out2, err2) = run(d.path(), cdir.path(), &["callers", "helper"]);
    assert!(err2.contains("cache load"), "second run reads: {err2}");
    assert_eq!(out1.lines().next(), out2.lines().next());
    assert!(out2.contains("app.main"), "{out2}");
    // an edit: the new call is there at once
    std::fs::write(d.path().join("lib.py"), "def helper():\n    return 2\n\ndef unused_one():\n    helper()\n    return 3\n").unwrap();
    let (out3, err3) = run(d.path(), cdir.path(), &["callers", "helper"]);
    assert!(!err3.contains("cache load"), "{err3}");
    assert!(out3.contains("lib.unused_one"), "{out3}");
    assert_eq!(cache_files(cdir.path()).len(), 2);
    // --no-cache neither reads nor writes
    let (_o, err4) = run(d.path(), cdir.path(), &["callers", "helper", "--no-cache"]);
    assert!(!err4.contains("cache"), "{err4}");
    assert_eq!(cache_files(cdir.path()).len(), 2);
}

// only the newest few graphs of a repository stay
#[test]
fn the_newest_graphs_stay() {
    let (d, _p) = project();
    let cdir = TempDir::new().unwrap();
    for i in 0..6 {
        std::fs::write(d.path().join("lib.py"), format!("def helper():\n    return {i}\n")).unwrap();
        run(d.path(), cdir.path(), &["stats"]);
    }
    assert_eq!(cache_files(cdir.path()).len(), 3);
}

// a worktree shares the main checkout's graph (keyed by git's common dir and file content)
#[test]
fn a_worktree_shares_the_graph() {
    let (d, _p) = project();
    common::git(d.path(), &["init", "-q", "-b", "main"]);
    common::git(d.path(), &["add", "-A"]);
    common::git(d.path(), &["commit", "-q", "-m", "x"]);
    let cdir = TempDir::new().unwrap();
    run(d.path(), cdir.path(), &["stats"]);
    let wt = d.path().join("wt");
    common::git(d.path(), &["worktree", "add", "-q", wt.to_str().unwrap()]);
    let (_o, err) = run(&wt, cdir.path(), &["stats"]);
    assert!(err.contains("cache load"), "{err}");
}
