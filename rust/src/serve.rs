//! `seamcheck serve`: an MCP server on stdin / stdout (serve.py).
//!
//! An agent asks seamcheck many small questions in a row; the server keeps
//! the loaded project and graph and answers from them. Before each tool call
//! it stats the project's files and config (mtime + size) and reloads only
//! when one changed, appeared or went: an edit between two calls is always
//! seen, an unchanged tree never reloads.
//!
//! The protocol is MCP 2025-06-18: JSON-RPC 2.0, one message per line;
//! initialize, ping, tools/list and tools/call. Every tool answers with one
//! text content holding JSON, the shape the matching `--json` prints.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::config::{self, truthy};
use crate::output::{dumps, dumps_line};
use crate::project::Project;
use crate::pyre::repr;
use crate::queries::Q;

pub const PROTOCOL: &str = "2025-06-18";

fn schema(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

/// The tools, in serve.py's order: (name, description, input schema).
pub fn tools() -> Vec<(&'static str, &'static str, Value)> {
    let depth = json!({"type": "integer", "minimum": 1, "description": "how many calls deep to walk (default 2)"});
    let fuzzy = json!({"type": "boolean", "description": "also follow name-guess edges (default: exact edges only)"});
    let since = json!({"type": "string", "description": "only files changed since this git ref (e.g. HEAD, origin/main)"});
    let spec = json!({"type": "string", "description": "`name` (glob ok), `Module.name` or `path/glob:func`"});
    vec![
        ("check", "Run the project's rules. Returns the findings: rule, severity, file, line, message, why, fix.",
         schema(json!({"only": {"type": "array", "items": {"type": "string"}, "description": "only these rule ids"},
                       "skip": {"type": "array", "items": {"type": "string"}, "description": "leave out these rule ids"},
                       "errors_only": {"type": "boolean"}}), &[])),
        ("gate", "May the working tree's change land? Rule errors anywhere, plus dead code, copies and do-nothing tests \
                  in the lines it touched, and a warning when a touched language resolves worse than at the ref. \
                  Returns {blocked, changed_files, findings}.",
         schema(json!({"since": {"type": "string", "description": "compare against this git ref (default HEAD)"},
                       "staged": {"type": "boolean", "description": "only what is staged"}}), &[])),
        ("callers", "Who calls a function, transitively. Each match: its exact and name-guess caller counts, the count of \
                     possible callers (calls by that name on a library-typed value), and the callers reached with their depth.",
         schema(json!({"name": spec, "depth": depth, "fuzzy": fuzzy,
                       "wide": {"type": "boolean", "description": "also list the possible callers (`possible_sites`) and \
                                follow name-guess edges: everything that may call it, ranked by tier"}}), &["name"])),
        ("callees", "What a function calls, transitively (same shape as callers).",
         schema(json!({"name": spec, "depth": depth, "fuzzy": fuzzy}), &["name"])),
        ("path", "The shortest call chain between two functions. An empty path with through_guesses_only = true: \
                  a chain exists only through name guesses (ask again with fuzzy).",
         schema(json!({"from": {"type": "string"}, "to": {"type": "string"}, "fuzzy": fuzzy}), &["from", "to"])),
        ("find", "Where functions are defined: id, file, lines, calls made, exact and name-guess callers.",
         schema(json!({"name": spec}), &["name"])),
        ("context", "Everything to know before editing a function, in one call: where it is, its source, each caller \
                     (exact first, then name guesses), what each of its calls reaches, and the tests that reach it.",
         schema(json!({"name": spec}), &["name"])),
        ("search", "Code search: lines matching a regex, each with the function it is in and that function's exact \
                    and name-guess caller counts. Over a workspace folder of several checkouts, it searches them all.",
         schema(json!({"pattern": {"type": "string", "description": "a Python regex"},
                       "globs": {"type": "array", "items": {"type": "string"}, "description": "only these files"},
                       "code": {"type": "boolean", "description": "leave out comments and string contents"},
                       "limit": {"type": "integer", "minimum": 1, "description": "default 200"}}), &["pattern"])),
        ("refs", "Where a name is defined, called and mentioned anywhere (scripts and config too).",
         schema(json!({"name": {"type": "string"}}), &["name"])),
        ("impact", "What a change could break: the callers and bridged views of the targets (or of the functions \
                    changed since a ref), by area.",
         schema(json!({"targets": {"type": "array", "items": {"type": "string"}, "description": "function specs or file paths"},
                       "since": {"type": "string", "description": "also the functions changed since this git ref"},
                       "staged": {"type": "boolean"},
                       "depth": {"type": "integer", "minimum": 1, "description": "default 4"}, "fuzzy": fuzzy}), &[])),
        ("stats", "Size, load time, and per language how call sites resolve (project_exact: of the calls into project \
                   code, the share that is certain).", schema(json!({}), &[])),
        ("unused", "Product functions nothing calls and no other file names.", schema(json!({"since": since}), &[])),
        ("tests", "Every test: how much product code it reaches and whether it reads product source \
                   (reach 0 or reads_source: a test that tests nothing).", schema(json!({"since": since}), &[])),
    ]
}

/// A call the tool can't answer (no such function): an error result, not a
/// protocol error, so the agent reads why (serve.ToolError).
pub struct ToolError(pub String);

type Stamps = HashMap<PathBuf, Option<(u128, u64)>>;

fn stamp(path: &Path) -> Option<(u128, u64)> {
    let md = std::fs::metadata(path).ok()?;
    let ns = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some((ns, md.len()))
}

pub struct Server {
    pub root: PathBuf,
    pub extra: Vec<String>,
    loaded: Option<Project>,
    stamps: Stamps,
    /// how many times a call loaded the project (the reload test counts them)
    pub loads: usize,
}

fn arg_str<'v>(args: &'v Map<String, Value>, k: &str) -> Option<&'v str> {
    args.get(k).and_then(|v| v.as_str())
}

fn arg_int(args: &Map<String, Value>, k: &str, d: i64) -> i64 {
    match args.get(k) {
        Some(v) => v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).unwrap_or(d),
        None => d,
    }
}

fn arg_list(args: &Map<String, Value>, k: &str) -> Option<Vec<String>> {
    match args.get(k)? {
        Value::Array(a) => Some(a.iter().filter_map(|x| x.as_str().map(String::from)).collect()),
        Value::String(s) => Some(s.chars().map(String::from).collect()), // (Python iterates a str)
        _ => None,
    }
}

impl Server {
    pub fn new(root: &Path, extra: &[String]) -> Server {
        Server { root: root.to_path_buf(), extra: extra.to_vec(), loaded: None, stamps: HashMap::new(), loads: 0 }
    }

    /// mtime + size of every file a load reads: the project's files as listed
    /// now (one appeared or went: a change too) and its config.
    fn stamps_now(&self) -> Stamps {
        let (cfg, _) = config::load(&self.root, &self.extra);
        let pc = cfg.table("project");
        let include = config::list_or(pc, "include", &["**/*"]);
        let mut exclude = crate::walk::default_exclude(&include);
        exclude.extend(config::list_or(pc, "exclude", &[]));
        let mut out: Stamps = HashMap::new();
        for rel in crate::walk::list_files(&self.root, &include, &exclude) {
            let path = self.root.join(&rel);
            out.insert(path.clone(), stamp(&path));
        }
        for path in config::files_for(&self.root, &self.extra) {
            let path = self.root.join(path);
            out.insert(path.clone(), stamp(&path));
        }
        out
    }

    fn project(&mut self) -> &mut Project {
        let now = self.stamps_now(); // (before the load: an edit during it is seen by the next call)
        if self.loaded.is_none() || now != self.stamps {
            let p = Project::load(&self.root, &self.extra);
            let _ = p.graph(); // (the graph the rules and the queries share)
            self.loaded = Some(p);
            self.loads += 1;
            self.stamps = now;
        }
        self.loaded.as_mut().unwrap()
    }

    /// One tool call's answer (serve.Server.call).
    pub fn call(&mut self, name: &str, args: &Map<String, Value>) -> Result<Value, ToolError> {
        let (_, _, sch) = tools().into_iter().find(|t| t.0 == name).ok_or_else(|| ToolError(format!("unknown tool {name}")))?;
        let props = sch["properties"].as_object().cloned().unwrap_or_default();
        let missing: Vec<&str> =
            sch["required"].as_array().into_iter().flatten().filter_map(|k| k.as_str()).filter(|k| !args.contains_key(*k)).collect();
        let unknown: Vec<&str> = args.keys().filter(|k| !props.contains_key(*k)).map(|k| k.as_str()).collect();
        if !missing.is_empty() || !unknown.is_empty() {
            let mut parts = Vec::new();
            if !missing.is_empty() {
                parts.push(format!("missing {}", missing.join(", ")));
            }
            if !unknown.is_empty() {
                parts.push(format!("unknown {}", unknown.join(", ")));
            }
            return Err(ToolError(parts.join("; ")));
        }
        let extra = self.extra.clone();
        let p = self.project();
        let out = run_tool(p, name, args, &extra);
        p.gate_changed = None; // (a gate's scope: the next check sees everything)
        out
    }

    /// One JSON-RPC message in, the reply out (None for a notification).
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let Value::Object(m) = msg else {
            return Some(error(Value::Null, -32600, "one JSON-RPC object per line (batches are not part of MCP 2025-06-18)"));
        };
        let mid = m.get("id").cloned().unwrap_or(Value::Null);
        let method = m.get("method").and_then(|x| x.as_str());
        let empty = Map::new();
        let params = m.get("params").and_then(|x| x.as_object()).unwrap_or(&empty);
        let method = match method {
            Some(x) if m.contains_key("id") => x,
            _ => return None, // a notification (initialized, cancelled), or a response to nothing we asked
        };
        match method {
            "initialize" => Some(result(
                mid,
                json!({"protocolVersion": PROTOCOL, "capabilities": {"tools": {"listChanged": false}},
                       "serverInfo": {"name": "seamcheck", "version": crate::VERSION},
                       "instructions": format!("seamcheck's call graph and rules for the project at {}. Queries walk exact edges; `fuzzy` adds name guesses.",
                                               self.root.to_string_lossy())}),
            )),
            "ping" => Some(result(mid, json!({}))),
            "tools/list" => Some(result(
                mid,
                json!({"tools": tools().into_iter().map(|(n, d, s)| json!({"name": n, "description": d, "inputSchema": s})).collect::<Vec<_>>()}),
            )),
            "tools/call" => {
                let name = params.get("name");
                let names: Vec<&str> = tools().iter().map(|t| t.0).collect();
                let Some(name) = name.and_then(|n| n.as_str()).filter(|n| names.contains(n)) else {
                    let shown = match name {
                        Some(Value::String(s)) => repr(s),
                        None | Some(Value::Null) => "None".into(),
                        Some(other) => other.to_string(),
                    };
                    return Some(error(mid, -32602, &format!("unknown tool {shown}; tools: {}", names.join(", "))));
                };
                let args = params.get("arguments").and_then(|a| a.as_object()).cloned().unwrap_or_default();
                let got = catch_unwind(AssertUnwindSafe(|| self.call(name, &args)));
                let (out, bad) = match got {
                    Ok(Ok(v)) => (v, false),
                    Ok(Err(ToolError(e))) => (json!({"error": e}), true),
                    // a crash in one query must not end the session
                    Err(e) => {
                        let why = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()));
                        (json!({"error": format!("panic: {}", why.unwrap_or_default())}), true)
                    }
                };
                Some(result(mid, json!({"content": [{"type": "text", "text": dumps(&out)}], "isError": bad})))
            }
            other => Some(error(mid, -32601, &format!("method not found: {other}"))),
        }
    }
}

/// serve._since: only the rows of files changed since the `since` ref, when one is given.
fn since_rows(p: &Project, args: &Map<String, Value>, rows: Vec<Value>) -> Value {
    match arg_str(args, "since").filter(|s| !s.is_empty()) {
        Some(r) => {
            let changed = crate::tidy::changed_files(p, r);
            Value::Array(rows.into_iter().filter(|x| x["file"].as_str().is_some_and(|f| changed.contains(f))).collect())
        }
        None => Value::Array(rows),
    }
}

fn run_tool(p: &mut Project, name: &str, args: &Map<String, Value>, extra: &[String]) -> Result<Value, ToolError> {
    if name == "gate" {
        let r = arg_str(args, "since").filter(|s| !s.is_empty()).unwrap_or("HEAD").to_string();
        let rep = crate::gate::gate_with(p, &r, truthy(args.get("staged")), extra, &crate::tidy::exactness::Book::default());
        let findings = crate::tidy::cli::gate_findings(p, &rep, &r);
        return Ok(json!({"blocked": rep.failed(), "changed_files": rep.changed_files, "findings": findings}));
    }
    let p: &Project = p;
    let g = p.graph();
    let q = Q::new(p, &g);
    let fuzzy = truthy(args.get("fuzzy"));
    Ok(match name {
        "check" => {
            let (only, skip) = (arg_list(args, "only"), arg_list(args, "skip"));
            let mut found = crate::rules::run_rules(p, only.as_deref(), skip.as_deref());
            if truthy(args.get("errors_only")) {
                found.retain(|f| f.severity == "error");
            }
            let whys: HashMap<&str, (&str, &str)> = p
                .config
                .rules()
                .map(|r| (config::id(r), (config::s(r, "why").unwrap_or(""), config::s(r, "fix").unwrap_or(""))))
                .collect();
            Value::Array(
                found
                    .iter()
                    .map(|f| {
                        let (why, fix) = whys.get(f.rule.as_str()).copied().unwrap_or(("", ""));
                        crate::output::flat(f, why, fix)
                    })
                    .collect(),
            )
        }
        "callers" | "callees" => {
            let spec = arg_str(args, "name").unwrap_or("");
            let rev = name == "callers";
            let got = q.walk(spec, arg_int(args, "depth", 2).max(0) as usize, rev, fuzzy, rev && truthy(args.get("wide")));
            if got.is_empty() {
                return Err(ToolError(format!("no function matches {}", repr(spec))));
            }
            Value::Array(got)
        }
        "path" => {
            let (chain, through) = q.shortest(arg_str(args, "from").unwrap_or(""), arg_str(args, "to").unwrap_or(""), fuzzy);
            json!({"path": chain.iter().map(|f| Value::Object(q.where_(f))).collect::<Vec<_>>(), "through_guesses_only": through})
        }
        "find" => Value::Array(q.find(arg_str(args, "name").unwrap_or(""))),
        "context" => Value::Array(q.context(arg_str(args, "name").unwrap_or(""), 80, 4)),
        "search" => {
            let globs = arg_list(args, "globs").unwrap_or_default();
            let limit = arg_int(args, "limit", 200).max(1) as usize;
            Value::Array(q.search(arg_str(args, "pattern").unwrap_or(""), &globs, truthy(args.get("code")), limit).map_err(ToolError)?)
        }
        "impact" => {
            let mut start = q.target_functions(&arg_list(args, "targets").unwrap_or_default());
            if let Some(since) = arg_str(args, "since").filter(|s| !s.is_empty()) {
                start.extend(q.changed_functions(since, truthy(args.get("staged"))));
            }
            if start.is_empty() {
                return Err(ToolError("nothing matches: give targets (function specs or files) or since (a git ref)".into()));
            }
            let (by, own) = q.impact(&start, arg_int(args, "depth", 4).max(0) as usize, fuzzy);
            let areas: Map<String, Value> = by
                .iter()
                .map(|(k, v)| {
                    let rows = v
                        .iter()
                        .map(|(d, fid)| {
                            let mut w = q.where_(fid);
                            w.insert("depth".into(), json!(d));
                            Value::Object(w)
                        })
                        .collect();
                    (k.clone(), Value::Array(rows))
                })
                .collect();
            json!({"start": start, "own_areas": own, "areas": areas})
        }
        "stats" => q.stats(),
        "refs" => {
            let t = crate::tidy::Tidy::new(p, &g);
            crate::tidy::refs(&t, arg_str(args, "name").unwrap_or(""), &crate::tidy::Corpus::new(p))
        }
        "unused" => {
            let t = crate::tidy::Tidy::new(p, &g);
            let rows = crate::tidy::dead::unused(&t, &crate::tidy::Corpus::new(p)).iter().map(|r| r.json()).collect();
            since_rows(p, args, rows)
        }
        "tests" => {
            let t = crate::tidy::Tidy::new(p, &g);
            since_rows(p, args, crate::tidy::audit::test_audit(&t, None).iter().map(|r| r.json()).collect())
        }
        other => return Err(ToolError(format!("unknown tool {other}"))),
    })
}

fn result(mid: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": mid, "result": result})
}

fn error(mid: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": mid, "error": {"code": code, "message": message}})
}

/// Serve stdin / stdout until stdin closes.
pub fn main(root: &Path, extra: &[String]) -> i32 {
    std::panic::set_hook(Box::new(|_| {})); // (a tool's panic is its error result, not stderr noise)
    let mut srv = Server::new(root, extra);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(line) {
            Err(_) => Some(error(Value::Null, -32700, "parse error: one JSON-RPC message per line")),
            Ok(msg) => srv.handle(&msg),
        };
        if let Some(r) = reply {
            let _ = writeln!(out, "{}", dumps_line(&r));
            let _ = out.flush();
        }
    }
    0
}
