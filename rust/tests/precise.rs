//! Precise mode (test_precise.py): a language server settles the calls the
//! static resolver guesses. The client is tested against test_precise.py's
//! fake LSP server (run by python3; skipped without it); each real server
//! has one test, skipped where it isn't installed.

use std::io::Cursor;
use std::path::Path;

use seamcheck::graph::precise::{self, from_u16, lsp, to_u16};
use seamcheck::graph::Graph;
use seamcheck::project::Project;

// A tiny LSP server: a definition is the first `def NAME(` in the root's .py
// files, except `dumps`, which lives in a library outside the root. Modes:
// "crash" exits at once, "hang" never answers a definition. Each definition
// request is logged (one line) to the file named by argv[2].
const FAKE: &str = r#"
import json, os, re, sys
mode, log = sys.argv[1], sys.argv[2]
root, docs = None, {}
if mode == "crash":
    sys.stderr.write("fake: boom\n")
    sys.exit(3)

def read():
    n = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        k, _, v = line.partition(b":")
        if k.lower() == b"content-length":
            n = int(v)
    return json.loads(sys.stdin.buffer.read(n))

def send(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()

def where(name):
    if name == "dumps":
        return "file:///usr/lib/python3/json/__init__.py", 230, 4
    for dp, _, fs in sorted(os.walk(root)):
        for f in sorted(fs):
            if f.endswith(".py"):
                lines = open(os.path.join(dp, f)).read().split("\n")
                for i, l in enumerate(lines):
                    if re.match(r"\s*def " + name + r"\(", l):
                        return "file://" + os.path.join(dp, f), i, l.index(name)
    return None

while (msg := read()) is not None:
    m, p = msg.get("method"), msg.get("params") or {}
    if m == "initialize":
        root = p["rootPath"]
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {"capabilities": {"definitionProvider": True}}})
        send({"jsonrpc": "2.0", "id": 99, "method": "workspace/configuration", "params": {"items": [{}]}})
    elif m == "textDocument/didOpen":
        docs[p["textDocument"]["uri"]] = p["textDocument"]["text"].split("\n")
    elif m == "textDocument/definition":
        with open(log, "a") as fh:
            fh.write("q\n")
        if mode == "hang":
            continue
        line = docs[p["textDocument"]["uri"]][p["position"]["line"]]
        name = re.match(r"\w+", line[p["position"]["character"]:]).group(0)
        hit = where(name)
        pos = {"line": hit[1], "character": hit[2]} if hit else None
        send({"jsonrpc": "2.0", "id": msg["id"], "result": hit and [{"uri": hit[0], "range": {"start": pos, "end": pos}}]})
    elif m == "shutdown":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    elif m == "exit":
        break
"#;

const FILES: &[(&str, &str)] = &[
    ("shapes.py", "class Circle:\n    def area(self):\n        return 3\n\n\nclass Square:\n    def area(self):\n        return 4\n"),
    ("util.py", "def helper():\n    return 1\n\n\ndef helper2():\n    return 2\n"),
    ("main.py", "from util import helper2\n\n\ndef use(x, s):\n    helper2()\n    x.area()\n    s.dumps()\n    helper()\n"),
];

fn write(root: &Path, files: &[(&str, &str)]) {
    for (name, body) in files {
        let p = root.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
}

fn python() -> Option<String> {
    precise::which("python3").map(|v| v[0].clone())
}

/// A temp project whose .py server is FAKE in the given mode.
struct Fixture {
    dir: tempfile::TempDir,
    log: String,
}

impl Fixture {
    fn new(mode: &str, extra: &str) -> Option<Fixture> {
        let py = python()?;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let log = root.join(".fake.log").to_string_lossy().into_owned();
        let fake = root.join(".fake_server.py");
        std::fs::write(&fake, FAKE).unwrap();
        write(root, FILES);
        let toml = format!(
            "[project]\nexclude = [\".fake*\"]\n[precise]\nservers = {{ \".py\" = [{py:?}, {:?}, \"{mode}\", {log:?}] }}\n{extra}",
            fake.to_string_lossy()
        );
        std::fs::write(root.join("seamcheck.toml"), toml).unwrap();
        Some(Fixture { dir, log })
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn load(&self, precise: bool, cache: Option<&Path>) -> Graph {
        let p = Project::load(self.root(), &[]);
        Graph::load_precise(&p, true, precise, cache)
    }

    fn asked(&self) -> usize {
        std::fs::read_to_string(&self.log).map(|s| s.lines().count()).unwrap_or(0)
    }
}

fn edges(g: &Graph, suffix: &str) -> Vec<(String, bool)> {
    let ids: Vec<&String> = g.funcs.keys().filter(|f| f.ends_with(suffix)).collect();
    assert_eq!(ids.len(), 1, "{suffix}: {ids:?}");
    let mut v: Vec<(String, bool)> = g.edges.get(ids[0]).into_iter().flatten().map(|(t, _, e)| (t.clone(), *e)).collect();
    v.sort();
    v
}

fn has(v: &[(String, bool)], t: &str, exact: bool) -> bool {
    v.iter().any(|(x, e)| x == t && *e == exact)
}

/// FramingTest.test_messages_round_trip_and_tsserver_newlines_are_skipped
#[test]
fn messages_round_trip_and_tsserver_newlines_are_skipped() {
    let mut bytes = lsp::frame(&serde_json::json!({"id": 1}));
    bytes.extend(b"\n");
    bytes.extend(lsp::frame(&serde_json::json!({"id": 2, "result": [1]})));
    bytes.extend(b"Content-Length: 40\r\n\r\n{\"cut");
    let mut r = Cursor::new(bytes);
    assert_eq!(lsp::read_message(&mut r), Some(serde_json::json!({"id": 1})));
    assert_eq!(lsp::read_message(&mut r), Some(serde_json::json!({"id": 2, "result": [1]})));
    assert_eq!(lsp::read_message(&mut r), None); // a body cut short: the server died
}

/// FramingTest.test_columns_count_utf16_units
#[test]
fn columns_count_utf16_units() {
    let line = "x = \"\u{1F600}\"; y.area()";
    let col = line[..line.find("area").unwrap()].chars().count();
    assert_eq!(to_u16(line, col), col + 1); // the emoji is two units
    assert_eq!(from_u16(line, col + 1), col);
}

/// FakeServerTest.test_guesses_become_exact_or_library_and_counts_follow
#[test]
fn guesses_become_exact_or_library_and_counts_follow() {
    let Some(fx) = Fixture::new("ok", "") else { return };
    let (st, p) = (fx.load(false, None), fx.load(true, None));
    assert!(has(&edges(&st, "main.use"), "shapes.Square.area", false));
    let got = edges(&p, "main.use");
    assert!(has(&got, "shapes.Circle.area", true), "{got:?}"); // x.area(): the server's one answer
    assert!(!has(&got, "shapes.Square.area", false));
    assert!(has(&got, "util.helper", true)); // helper(): nothing statically (no import), found
    // every call is still counted once: the totals don't move
    let sum = |g: &Graph| g.resolution[".py"][..3].iter().sum::<usize>();
    assert_eq!(sum(&st), sum(&p));
    let s = p.precise.as_ref().unwrap();
    assert_eq!(s.exact_from_guess, 1);
    assert_eq!(s.exact_from_nothing, 1);
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    assert!(precise::summary(s).contains("1 guesses and 1 nothings made exact"));
}

/// FakeServerTest.test_a_library_answer_drops_the_guesses
#[test]
fn a_library_answer_drops_the_guesses() {
    let Some(fx) = Fixture::new("ok", "") else { return };
    let shapes = fx.root().join("shapes.py");
    let mut text = std::fs::read_to_string(&shapes).unwrap();
    text.push_str("\n\nclass Codec:\n    def dumps(self):\n        pass\n\n\nclass Other:\n    def dumps(self):\n        pass\n");
    std::fs::write(&shapes, text).unwrap();
    let (st, p) = (fx.load(false, None), fx.load(true, None));
    assert!(has(&edges(&st, "main.use"), "shapes.Codec.dumps", false));
    assert!(!edges(&p, "main.use").iter().any(|e| e.0.contains("dumps")));
    assert_eq!(p.precise.as_ref().unwrap().library, 1);
    assert_eq!(p.resolution[".py"][1], st.resolution[".py"][1] - 2); // area and dumps
}

/// FakeServerTest.test_a_crashing_server_is_a_warning_and_the_graph_stays_static
#[test]
fn a_crashing_server_is_a_warning_and_the_graph_stays_static() {
    let Some(fx) = Fixture::new("crash", "") else { return };
    let (st, p) = (fx.load(false, None), fx.load(true, None));
    assert_eq!(p.edges, st.edges);
    let w = &p.precise.as_ref().unwrap().warnings;
    assert_eq!(w.len(), 1);
    assert!(w[0].contains("code 3"), "{w:?}");
    // no config error: the run doesn't fail
    assert!(Project::load(fx.root(), &[]).config_problems.is_empty());
}

/// FakeServerTest.test_a_server_that_never_answers_is_given_up_on
#[test]
fn a_server_that_never_answers_is_given_up_on() {
    let Some(fx) = Fixture::new("hang", "wait = 1\ntimeout = 1\n") else { return };
    let (st, p) = (fx.load(false, None), fx.load(true, None));
    assert_eq!(p.edges, st.edges);
    assert!(p.precise.as_ref().unwrap().warnings[0].contains("no answer"));
}

/// FakeServerTest.test_a_configured_server_that_is_not_installed_is_a_warning
#[test]
fn a_configured_server_that_is_not_installed_is_a_warning() {
    let Some(fx) = Fixture::new("ok", "") else { return };
    std::fs::write(fx.root().join("seamcheck.toml"), "[precise]\nenabled = true\nservers = { \".py\" = [\"no-such-server-x9\"] }\n").unwrap();
    let p = Project::load(fx.root(), &[]);
    let g = Graph::load(&p); // (enabled by config)
    assert_eq!(g.precise.unwrap().warnings, vec!["precise: no-such-server-x9 (.py) is not installed".to_string()]);
}

/// FakeServerTest.test_a_warm_run_asks_nothing
#[test]
fn a_warm_run_asks_nothing() {
    let Some(fx) = Fixture::new("ok", "") else { return };
    let cache = tempfile::tempdir().unwrap();
    let p = fx.load(true, Some(cache.path()));
    let n = fx.asked();
    assert!(n > 0);
    // another file changes: main.py's answers come from the cache
    let util = fx.root().join("util.py");
    let mut text = std::fs::read_to_string(&util).unwrap();
    text.push_str("\n\ndef helper3():\n    return 3\n");
    std::fs::write(&util, text).unwrap();
    let again = fx.load(true, Some(cache.path()));
    assert_eq!(fx.asked(), n);
    assert_eq!(edges(&again, "main.use"), edges(&p, "main.use"));
    let s = again.precise.as_ref().unwrap();
    assert_eq!(s.cached, s.asked);
}

/// FakeServerTest.test_static_mode_never_starts_a_server
#[test]
fn static_mode_never_starts_a_server() {
    let Some(fx) = Fixture::new("ok", "") else { return };
    let p = fx.load(false, None);
    assert_eq!(fx.asked(), 0);
    assert!(p.precise.is_none());
}

// --- the real servers: one case each, where the server is installed -----------------

fn real(exe: &str, files: &[(&str, &str)], right: &str, wrong: &str) {
    if precise::which(exe).is_none() {
        eprintln!("{exe} is not installed: skipped");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    write(d.path(), files);
    let p = Project::load(d.path(), &[]);
    let st = Graph::load_precise(&p, true, false, None);
    assert_eq!(edges(&st, ".total"), vec![(right.to_string(), false), (wrong.to_string(), false)]);
    let pg = Graph::load_precise(&p, true, true, None);
    assert_eq!(pg.precise.as_ref().unwrap().warnings, Vec::<String>::new());
    assert_eq!(edges(&pg, ".total"), vec![(right.to_string(), true)]);
}

/// RealServerTest.test_pyright
#[test]
fn pyright() {
    real(
        "pyright-langserver",
        &[("shapes.py", "class Circle:\n    def area(self):\n        return 3\n\n\nclass Square:\n    def area(self):\n        return 4\n\n\ndef total():\n    shapes = {\"c\": Circle()}\n    return shapes[\"c\"].area()\n")],
        "shapes.Circle.area",
        "shapes.Square.area",
    );
}

/// RealServerTest.test_rust_analyzer
#[test]
fn rust_analyzer() {
    real(
        "rust-analyzer",
        &[
            ("Cargo.toml", "[package]\nname = \"shapes\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
            ("src/main.rs", "struct Circle;\nstruct Square;\n\nimpl Circle {\n    fn area(&self) -> u32 { 3 }\n}\n\nimpl Square {\n    fn area(&self) -> u32 { 4 }\n}\n\nfn total() -> u32 {\n    let shapes = vec![Circle];\n    shapes[0].area()\n}\n\nfn main() {\n    println!(\"{}\", total());\n}\n"),
        ],
        "src/main.Circle.area",
        "src/main.Square.area",
    );
}

/// RealServerTest.test_gopls
#[test]
fn gopls() {
    real(
        "gopls",
        &[
            ("go.mod", "module example.com/shapes\n\ngo 1.21\n"),
            ("main.go", "package main\n\ntype Circle struct{}\ntype Square struct{}\n\nfunc (c *Circle) Area() int { return 3 }\nfunc (s *Square) Area() int { return 4 }\n\nfunc total() int {\n\tshapes := map[string]*Circle{\"c\": {}}\n\treturn shapes[\"c\"].Area()\n}\n\nfunc main() { println(total()) }\n"),
        ],
        "main.Circle.Area",
        "main.Square.Area",
    );
}

/// RealServerTest.test_tsserver
#[test]
fn tsserver() {
    real(
        "tsserver",
        &[("shapes.ts", "class Circle {\n  area(): number { return 3 }\n}\n\nclass Square {\n  area(): number { return 4 }\n}\n\nexport function total(): number {\n  const shapes = [new Circle()]\n  return shapes[0].area()\n}\n")],
        "shapes.Circle.area",
        "shapes.Square.area",
    );
}
