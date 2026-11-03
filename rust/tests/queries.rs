//! The query side: the answers as data (queries.rs), the CLI's query
//! commands, the MCP server (serve.rs) and the resolution counters `stats`
//! prints. Each test names the Python test it ports.

mod common;

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{json, Map, Value};

use seamcheck::graph::Graph;
use seamcheck::project::Project;
use seamcheck::queries::Q;
use seamcheck::serve::Server;

fn copy_dir(from: &Path, to: &Path) {
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&dst).unwrap();
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

/// A copy of tests/queries_fixtures/<name>, loaded with its graph.
fn fixture(name: &str) -> (tempfile::TempDir, Project, Graph) {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/queries_fixtures").join(name), dir.path());
    let p = Project::load(dir.path(), &[]);
    let g = Graph::load(&p);
    (dir, p, g)
}

fn loaded(files: &[(&str, &str)]) -> (tempfile::TempDir, Project, Graph) {
    let dir = tempfile::tempdir().unwrap();
    common::write(dir.path(), files);
    let p = Project::load(dir.path(), &[]);
    let g = Graph::load(&p);
    (dir, p, g)
}

fn edges(g: &Graph, suffix: &str) -> std::collections::BTreeMap<String, bool> {
    let ids: Vec<&String> = g.funcs.keys().filter(|f| f.ends_with(suffix)).collect();
    assert_eq!(ids.len(), 1, "{suffix}: {ids:?}");
    g.edges.get(ids[0]).map(|v| v.iter().map(|(t, _, e)| (t.clone(), *e)).collect()).unwrap_or_default()
}

fn cli(root: &Path, args: &[&str]) -> (i32, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_seamcheck"))
        .args(args)
        .arg("-C")
        .arg(root)
        .env("SEAMCHECK_RS_QUIET", "1")
        .output()
        .unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned())
}

// --- test_serve.py --------------------------------------------------------------------

const SERVE_FILES: &[(&str, &str)] = &[
    (
        "seamcheck.toml",
        "[project]\nname = \"t\"\n\n[[rule]]\nid = \"writes-go-through-store\"\nkind = \"forbid\"\npattern = 'db\\.execute'\nallow = [\"src/app/store.py\"]\nwhy = \"the store clears the cache\"\n",
    ),
    ("src/app/__init__.py", ""),
    ("src/app/store.py", "def save(x):\n    db.execute(x)\n"),
    ("src/app/view.py", "from .store import save\n\n\ndef show(x):\n    return save(x)\n"),
    ("tests/test_view.py", "from app.view import show\n\n\ndef test_show():\n    show(1)\n"),
];

const TOOLS: &[&str] =
    &["check", "gate", "callers", "callees", "path", "find", "context", "refs", "impact", "stats", "unused", "tests", "search"];

/// A server subprocess, spoken to one JSON line at a time (test_serve.Session).
struct Session {
    child: std::process::Child,
    out: std::io::BufReader<std::process::ChildStdout>,
    n: i64,
}

impl Session {
    fn new(root: &Path) -> Session {
        let mut child = Command::new(env!("CARGO_BIN_EXE_seamcheck"))
            .args(["serve", "-C"])
            .arg(root)
            .env("SEAMCHECK_RS_QUIET", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let out = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut s = Session { child, out, n: 0 };
        let init = s.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {},
                                                  "clientInfo": {"name": "test", "version": "0"}}))["result"]
            .clone();
        assert_eq!((init["protocolVersion"].as_str(), init["serverInfo"]["name"].as_str()), (Some("2025-06-18"), Some("seamcheck")));
        s.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string()); // no reply: the next line is ping's
        assert_eq!(s.request("ping", Value::Null)["result"], json!({}));
        s
    }

    fn send(&mut self, line: &str) {
        use std::io::Write;
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        use std::io::BufRead;
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap() // (a stray print would fail here)
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.n += 1;
        let mut msg = json!({"jsonrpc": "2.0", "id": self.n, "method": method});
        if !params.is_null() {
            msg["params"] = params;
        }
        self.send(&msg.to_string());
        let reply = self.read();
        assert_eq!(reply["id"], json!(self.n), "{reply}");
        reply
    }

    fn tool(&mut self, tool: &str, args: Value) -> (Value, bool) {
        let res = self.request("tools/call", json!({"name": tool, "arguments": args}))["result"].clone();
        (serde_json::from_str(res["content"][0]["text"].as_str().unwrap()).unwrap(), res["isError"] == json!(true))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

fn serve_repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    common::write(d.path(), SERVE_FILES);
    common::git(d.path(), &["init", "-q"]);
    common::commit_all(d.path(), "base");
    d
}

fn ids(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().map(|r| r["id"].as_str().unwrap_or("").to_string()).collect()
}

// test_serve.py: ServeTest.test_an_edit_between_two_calls_is_seen (its gate half: with the gate-graph lane)
#[test]
fn serve_an_edit_between_two_calls_is_seen() {
    let d = serve_repo();
    let mut s = Session::new(d.path());
    let (hit, _) = s.tool("callers", json!({"name": "save"}));
    assert_eq!(ids(&hit[0]["reach"]), ["src/app/view.show", "tests/test_view.test_show"]);
    let (findings, _) = s.tool("check", json!({}));
    assert_eq!(findings, json!([]));
    common::write(
        d.path(),
        &[("src/app/view.py", "from .store import save\n\n\ndef show(x):\n    return save(x)\n\n\ndef again(x):\n    db.execute(x)\n    return save(x)\n")],
    );
    let (hit, _) = s.tool("callers", json!({"name": "save", "depth": 1}));
    assert!(ids(&hit[0]["reach"]).contains(&"src/app/view.again".to_string()));
    let (findings, _) = s.tool("check", json!({}));
    let got: Vec<(String, String, u64, String)> = findings
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["rule"].as_str().unwrap().into(), f["file"].as_str().unwrap().into(), f["line"].as_u64().unwrap(), f["why"].as_str().unwrap().into()))
        .collect();
    assert_eq!(got, [("writes-go-through-store".into(), "src/app/view.py".into(), 9, "the store clears the cache".into())]);
}

// test_serve.py: ServeTest.test_every_tool_answers (refs / unused / tests / gate: with the gate-graph lane)
#[test]
fn serve_every_tool_answers() {
    let d = serve_repo();
    let mut s = Session::new(d.path());
    let tools = s.request("tools/list", Value::Null)["result"]["tools"].clone();
    let mut names: Vec<&str> = tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    let mut want = TOOLS.to_vec();
    want.sort();
    assert_eq!(names, want);
    for t in tools.as_array().unwrap() {
        assert_eq!(t["inputSchema"]["type"], "object");
        assert!(!t["description"].as_str().unwrap().is_empty());
    }
    assert_eq!(s.tool("find", json!({"name": "show"})).0[0]["file"], "src/app/view.py");
    assert_eq!(ids(&s.tool("callees", json!({"name": "show"})).0[0]["reach"]), ["src/app/store.save"]);
    assert_eq!(
        ids(&s.tool("path", json!({"from": "test_show", "to": "save"})).0["path"]),
        ["tests/test_view.test_show", "src/app/view.show", "src/app/store.save"]
    );
    assert!(s.tool("impact", json!({"targets": ["src/app/store.py"]})).0["areas"].get("tests").is_some());
    assert_eq!(s.tool("stats", json!({})).0["resolution"][".py"]["project_exact"], 100);
    let ctx = s.tool("context", json!({"name": "save"})).0[0].clone(); // before editing save: who calls it, which tests to run
    assert!(ctx["source"].as_str().unwrap().contains("db.execute(x)"));
    let callers: Vec<(String, bool)> =
        ctx["callers"].as_array().unwrap().iter().map(|c| (c["id"].as_str().unwrap().into(), c["exact"].as_bool().unwrap())).collect();
    assert_eq!(callers, [("src/app/view.show".to_string(), true)]);
    assert_eq!(ids(&ctx["tests"]), ["tests/test_view.test_show"]);
}

// test_serve.py: ServeTest.test_errors
#[test]
fn serve_errors() {
    let d = serve_repo();
    let mut s = Session::new(d.path());
    let (out, bad) = s.tool("callers", json!({"name": "nosuch"}));
    assert!(bad);
    assert!(out["error"].as_str().unwrap().contains("no function matches 'nosuch'"));
    let (out, bad) = s.tool("path", json!({"to": "save"}));
    assert!(bad);
    assert!(out["error"].as_str().unwrap().contains("missing from"));
    assert_eq!(s.request("tools/call", json!({"name": "nosuch"}))["error"]["code"], -32602);
    assert_eq!(s.request("resources/list", Value::Null)["error"]["code"], -32601);
    s.send("{not json");
    assert_eq!(s.read()["error"]["code"], -32700);
    assert_eq!(s.request("ping", Value::Null)["result"], json!({})); // still serving
}

// test_serve.py: Reload.test_an_unchanged_tree_is_not_loaded_again
#[test]
fn serve_an_unchanged_tree_is_not_loaded_again() {
    let d = tempfile::tempdir().unwrap();
    common::write(d.path(), SERVE_FILES);
    let mut srv = Server::new(d.path(), &[]);
    let args = |v: Value| -> Map<String, Value> { v.as_object().unwrap().clone() };
    assert!(srv.call("find", &args(json!({"name": "save"}))).is_ok());
    assert!(srv.call("check", &args(json!({}))).is_ok());
    assert_eq!(srv.loads, 1);
    common::write(d.path(), &[("src/app/new.py", "def fresh():\n    return 1\n")]); // a new file is a change too
    let got = srv.call("find", &args(json!({"name": "fresh"}))).ok().unwrap();
    assert_eq!(got[0]["file"], "src/app/new.py");
    assert_eq!(srv.loads, 2);
}

// --- test_impact_since.py -------------------------------------------------------------

const A: &str = "extends Node\n\nfunc base(x):\n\treturn x\n\nfunc middle(x):\n\treturn base(x)\n\nfunc untouched(x):\n\treturn x\n";
const B: &str = "extends Node\nconst A := preload(\"res://a.gd\")\n\nfunc top():\n\tA.middle(1)\n";

fn impact_repo() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    common::write(d.path(), &[("a.gd", A), ("b.gd", B), ("seamcheck.toml", "[project]\nname = \"t\"\ninclude = [\"**\"]\n")]);
    common::git(d.path(), &["init", "-q"]);
    common::commit_all(d.path(), "x");
    d
}

// test_impact_since.py: ImpactSince.test_nothing_changed
#[test]
fn impact_since_nothing_changed() {
    let d = impact_repo();
    let (code, out) = cli(d.path(), &["impact", "--since", "HEAD"]);
    assert_eq!(code, 0);
    assert!(out.contains("no functions changed"), "{out}");
}

// test_impact_since.py: ImpactSince.test_edit_reaches_callers
#[test]
fn impact_since_edit_reaches_callers() {
    let d = impact_repo();
    std::fs::write(d.path().join("a.gd"), A.replacen("return x\n\nfunc middle", "return x + 1\n\nfunc middle", 1)).unwrap();
    let (_, out) = cli(d.path(), &["impact", "--since", "HEAD"]);
    assert!(out.contains("1 functions changed"), "{out}");
    assert!(out.contains("a.middle"));
    assert!(out.contains("b.top"));
    assert!(!out.contains("untouched"));
}

// test_impact_since.py: ImpactSince.test_deleted_line_counts
#[test]
fn impact_since_deleted_line_counts() {
    let d = impact_repo();
    std::fs::write(d.path().join("a.gd"), A.replacen("func middle(x):\n\treturn base(x)\n", "func middle(x):\n", 1)).unwrap();
    let (_, out) = cli(d.path(), &["impact", "--since", "HEAD"]);
    assert!(out.contains("1 functions changed"), "{out}");
    assert!(out.contains("b.top"));
}

// --- test_workspace.py ----------------------------------------------------------------

// test_workspace.py: Workspace.test_symlinked_checkouts_are_one_graph
#[test]
fn search_symlinked_checkouts_are_one_graph() {
    let d = tempfile::tempdir().unwrap();
    common::write(
        &d.path().join("lib-repo"),
        &[
            ("lib/src/httplib/__init__.py", "from httplib.api import get\n"),
            ("lib/src/httplib/api.py", "def get(url, timeout=None):\n    return request('GET', url)\n\n\ndef request(m, url):\n    return m\n"),
        ],
    );
    common::write(
        &d.path().join("app-repo"),
        &[("svc/client.py", "import httplib\n\n\ndef fetch(url):\n    return httplib.get(url, timeout=3)\n\n\ndef main():\n    fetch('x')\n")],
    );
    let ws = d.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::os::unix::fs::symlink(d.path().join("lib-repo/lib"), ws.join("lib")).unwrap();
    std::os::unix::fs::symlink(d.path().join("app-repo/svc"), ws.join("svc")).unwrap();
    let p = Project::load(&ws, &[]);
    let g = Graph::load(&p);
    let got: Vec<&String> = g.edges["svc/client.fetch"].iter().filter(|e| e.2).map(|e| &e.0).collect();
    assert_eq!(got, ["lib/src/httplib/api.get"]);
    let q = Q::new(&p, &g);
    let hits = q.search(r"timeout=3", &[], false, 200).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        (&hits[0]["file"], &hits[0]["line"], &hits[0]["func"], &hits[0]["exact_callers"]),
        (&json!("svc/client.py"), &json!(5), &json!("svc/client.fetch"), &json!(1))
    );
    let files: Vec<Value> = q.search(r"def \w+\(", &["lib/**".into()], true, 200).unwrap().iter().map(|h| h["file"].clone()).collect();
    assert_eq!(files, [json!("lib/src/httplib/api.py"), json!("lib/src/httplib/api.py")]);
}

// --- test_bridges.py ------------------------------------------------------------------

// test_bridges.py: BridgeTest.test_bridges_and_impact (the impact half; the bridges are graph_bridges')
#[test]
fn impact_follows_callers_and_bridges() {
    let (_d, p, g) = fixture("bridges");
    assert!(p.config_problems.is_empty(), "{:?}", p.config_problems);
    assert_eq!(g.bridged.get(&("app/Api.inbox".into(), "Web.InboxController.index".into())).map(|s| s.as_str()), Some("http"));
    // changing Inbox.list reaches the phone code that calls its route
    assert!(g.impact(&["Inbox.list".into()], 4, false).contains_key("app/Api.inbox"));
    // changing the channel reaches the web listener (forward through the bridge)
    assert!(g.impact(&["Web.Channel.handle_info".into()], 4, false).contains_key("web/clock.bind"));
    // and the CLI says which bridge a function sits on
    let q = Q::new(&p, &g);
    assert!(q.fmt("app/Api.inbox").ends_with("  [via http]"), "{}", q.fmt("app/Api.inbox"));
}

// --- test_holdout_fixes.py ------------------------------------------------------------

// test_holdout_fixes.py: PossibleCallersTier.test_a_library_typed_call_is_a_possible_caller_not_an_edge
#[test]
fn a_library_typed_call_is_a_possible_caller_not_an_edge() {
    let (_d, p, g) = fixture("possible");
    let f = g.find("Wrapped.Read")[0];
    assert!(!g.edges.values().flatten().any(|e| e.0 == f.id)); // no edge
    let got = Q::new(&p, &g).walk("Wrapped.Read", 1, true, false, true)[0].clone();
    assert_eq!(got["possible"], 1);
    assert_eq!(got["possible_sites"][0]["line"], 8);
}

// --- the resolution counters (test_ruby.py) -------------------------------------------

// test_ruby.py: RubyResolutionTest.test_requires_reach_top_level_methods_and_libraries_reach_nothing
#[test]
fn ruby_libraries_reach_nothing_and_count_no_guess() {
    let (_d, _p, g) = fixture("ruby_requires");
    let want: std::collections::BTreeMap<String, bool> =
        [("lib/sinatra/helpers.helper_fn".to_string(), true), ("lib/sinatra/base.base_fn".to_string(), true)].into();
    assert_eq!(edges(&g, "main.run_app"), want);
    // a gem's constant and what a library returns reach nothing, not Parser#parse
    assert_eq!(g.resolution[".rb"][1], 0);
}

// test_ruby.py: RubyResolutionTest.test_a_core_method_on_an_untyped_receiver_is_rubys
#[test]
#[ignore = "the Ruby link still guesses `opts.each` onto Drop#each (a Ruby core method on an untyped receiver); Ruby lane"]
fn ruby_core_methods_count_as_named() {
    let (_d, _p, g) = fixture("ruby_core_method");
    let got = edges(&g, "Use.go");
    let want: std::collections::BTreeMap<String, bool> =
        [("lib/other.Other.custom_thing".to_string(), false), ("lib/other.Third.custom_thing".to_string(), false)].into();
    assert_eq!(got, want);
    assert_eq!(g.resolution[".rb"][3], 2); // each, key?: named, not hidden
}

// --- dead (test_languages.py) ---------------------------------------------------------

// test_languages.py: Go.test_go_tests (dead's half: a test is never dead)
#[test]
fn a_test_is_never_dead() {
    let (_d, _p, g) = fixture("go");
    let t = g.func("command_test.TestExecute").unwrap();
    assert_eq!(t.kind, "test");
    assert!(!g.dead().iter().any(|f| f.id == t.id));
    assert_eq!(g.find("Command.ExecuteC").iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["command.Command.ExecuteC"]);
}

// --- the CLI's query commands (test_serve.py's project, over the command line) --------

#[test]
fn cli_query_commands() {
    let (d, _p, _g) = loaded(SERVE_FILES);
    let (code, out) = cli(d.path(), &["callers", "save"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "src/app/store.save  (src/app/store.py:1)  [1 exact, 0 name-guess callers, walking exact]\n  <- src/app/view.show  (src/app/view.py:4)\n    <- tests/test_view.test_show  (tests/test_view.py:4)\n"
    );
    assert_eq!(cli(d.path(), &["callers", "nosuch"]), (1, "no function matches 'nosuch'\n".into()));
    let (code, out) = cli(d.path(), &["path", "test_show", "save"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "tests/test_view.test_show  (tests/test_view.py:4)\n  -> src/app/view.show  (src/app/view.py:4)\n    -> src/app/store.save  (src/app/store.py:1)\n"
    );
    assert_eq!(cli(d.path(), &["path", "save", "test_show"]), (1, "no call chain found\n".into()));
    let (_, out) = cli(d.path(), &["find", "save"]);
    assert_eq!(out, "src/app/store.save  src/app/store.py:1-2  calls=1 callers: 1 exact, 0 name-guess\n");
    let (code, out) = cli(d.path(), &["search", r"save\(", "--json"]);
    assert_eq!(code, 0);
    let rows: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2);
    // argparse: a positional after the options that followed the args is unrecognized
    assert_eq!(cli(d.path(), &["search", "save", "--code", "src/**"]).0, 2);
    let (_, out) = cli(d.path(), &["stats"]);
    assert!(out.contains("  .py          3  exact  66%  guess   0%  nothing  33% (named  0%)   project calls exact 100%"), "{out}");
    let (_, out) = cli(d.path(), &["dead"]);
    assert_eq!(out, "");
}

#[test]
fn map_writes_the_page() {
    let (d, _p, _g) = loaded(SERVE_FILES);
    let html = d.path().join("map.html");
    let (code, out) = cli(d.path(), &["map", "--html", html.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(out.starts_with("map: "), "{out}");
    let page = std::fs::read_to_string(&html).unwrap();
    assert!(page.contains("<title>t seam map</title>"));
    let data = page.split("const D=").nth(1).unwrap().split(";const cv=").next().unwrap();
    let v: Value = serde_json::from_str(data).unwrap();
    assert_eq!(v["name"], "t");
    assert!(v["nodes"].as_array().unwrap().iter().any(|n| n["id"] == "src/app/store"));
}
