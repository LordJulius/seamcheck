//! Talking to the language servers (precise.py's framing, `_Conn`, `Lsp`,
//! `TsServer`, `_drive`, `_ready`): definition requests pipelined over stdio.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub const WINDOW: usize = 256; // requests in flight per server: enough to keep it busy, few enough to stay responsive
const BUSY: &[i64] = &[-32801, -32800, -32802]; // ContentModified, RequestCancelled, ServerCancelled

/// The server exited, or went quiet for longer than it may.
#[derive(Debug)]
pub struct Dead(pub String);

/// What one answer says: where the name is defined, an error, or "busy,
/// ask again" (rust-analyzer while it loads).
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Locs(Vec<(String, usize, usize)>),
    Error,
    Retry,
}

pub fn frame(obj: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(obj).unwrap_or_default();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend(body);
    out
}

/// The next framed message, `{}` for a body that isn't a JSON object, None
/// at EOF. tsserver ends each body with a newline the length may not count:
/// a blank line before the headers is skipped.
pub fn read_message(r: &mut impl BufRead) -> Option<Value> {
    let mut length: Option<usize> = None;
    loop {
        let mut line = Vec::new();
        if r.read_until(b'\n', &mut line).ok()? == 0 {
            return None;
        }
        let t = line.trim_ascii();
        if t.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        let (k, v) = match t.iter().position(|b| *b == b':') {
            Some(i) => (&t[..i], &t[i + 1..]),
            None => (t, &t[t.len()..]),
        };
        if k.trim_ascii().eq_ignore_ascii_case(b"content-length") {
            length = Some(std::str::from_utf8(v.trim_ascii()).ok()?.parse().ok()?);
        }
    }
    let n = length?;
    let mut body = vec![0u8; n];
    let mut got = 0;
    while got < n {
        match r.read(&mut body[got..]) {
            Ok(0) | Err(_) => return None,
            Ok(k) => got += k,
        }
    }
    match serde_json::from_slice::<Value>(&body) {
        Ok(v @ Value::Object(_)) => Some(v),
        _ => Some(json!({})),
    }
}

/// Python's `str(OSError)` of a process that won't start.
pub fn os_error(e: &std::io::Error, file: &str) -> String {
    match e.raw_os_error() {
        Some(n) => {
            let text = e.to_string();
            let desc = text.split(" (os error").next().unwrap_or(&text).to_string();
            format!("[Errno {n}] {desc}: {}", crate::pyre::repr(file))
        }
        None => e.to_string(),
    }
}

/// A server process, a thread reading its messages into a channel (and
/// answering the server's own requests, for LSP) and one keeping the tail
/// of its stderr (for the warning when it dies).
pub struct Conn {
    child: Child,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    rx: Receiver<Option<Value>>,
    dead: bool,
    err: Arc<Mutex<VecDeque<String>>>,
}

impl Conn {
    pub fn start(argv: &[String], cwd: &str, env: &[(&str, &str)], lsp: bool) -> std::io::Result<Conn> {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn()?;
        let stdin = Arc::new(Mutex::new(child.stdin.take()));
        let out = child.stdout.take().unwrap();
        let errp = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let w = stdin.clone();
        std::thread::spawn(move || {
            let mut r = BufReader::new(out);
            while let Some(msg) = read_message(&mut r) {
                if lsp && msg.get("method").is_some() && msg.get("id").is_some() {
                    // a server's own request: answered here, so it never waits on us
                    let items = if msg["method"] == "workspace/configuration" { msg["params"]["items"].as_array().cloned() } else { None };
                    let result = match items {
                        Some(xs) if !xs.is_empty() => Value::Array(vec![Value::Null; xs.len()]),
                        _ => Value::Null,
                    };
                    let reply = frame(&json!({"jsonrpc": "2.0", "id": msg["id"], "result": result}));
                    if let Some(s) = w.lock().unwrap().as_mut() {
                        let _ = s.write_all(&reply).and_then(|_| s.flush());
                    }
                } else if tx.send(Some(msg)).is_err() {
                    return;
                }
            }
            let _ = tx.send(None);
        });
        let err = Arc::new(Mutex::new(VecDeque::new()));
        let e2 = err.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(errp).split(b'\n').map_while(Result::ok) {
                let s = String::from_utf8_lossy(&line);
                let s = s.trim();
                if !s.is_empty() {
                    let mut q = e2.lock().unwrap();
                    q.push_back(s.chars().take(200).collect());
                    if q.len() > 3 {
                        q.pop_front();
                    }
                }
            }
        });
        Ok(Conn { child, stdin, rx, dead: false, err })
    }

    pub fn send(&mut self, data: &[u8]) -> Result<(), Dead> {
        let ok = match self.stdin.lock().unwrap().as_mut() {
            Some(s) => s.write_all(data).and_then(|_| s.flush()).is_ok(),
            None => false,
        };
        if ok {
            Ok(())
        } else {
            Err(Dead(format!("exited ({})", self.why())))
        }
    }

    /// The next message, or None when none came in time.
    pub fn poll(&mut self, timeout: f64) -> Result<Option<Value>, Dead> {
        if self.dead {
            return Err(Dead(format!("exited ({})", self.why())));
        }
        match self.rx.recv_timeout(Duration::from_secs_f64(timeout.max(0.01))) {
            Ok(Some(m)) => Ok(Some(m)),
            Ok(None) | Err(RecvTimeoutError::Disconnected) => {
                self.dead = true;
                Err(Dead(format!("exited ({})", self.why())))
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
        }
    }

    fn why(&mut self) -> String {
        // (a moment for an exiting process to be reaped, as Python's poll() sees it gone)
        let code = self.child.try_wait().ok().flatten();
        let tail = self.err.lock().unwrap().back().cloned().unwrap_or_default();
        match code {
            Some(st) => {
                use std::os::unix::process::ExitStatusExt;
                let c = st.code().unwrap_or_else(|| -st.signal().unwrap_or(0));
                if tail.is_empty() {
                    format!("code {c}")
                } else {
                    format!("code {c}: {tail}")
                }
            }
            None if !tail.is_empty() => tail,
            None => "closed its output".into(),
        }
    }

    fn wait_for(&mut self, secs: f64) -> bool {
        let end = Instant::now() + Duration::from_secs_f64(secs);
        loop {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// After a goodbye: a moment to exit on its own, then killed.
    pub fn kill(&mut self) {
        self.stdin.lock().unwrap().take();
        if !self.wait_for(2.0) {
            let _ = self.child.kill();
            self.wait_for(5.0);
        }
    }
}

const TS_KINDS: &[(&str, &str)] = &[(".tsx", "TSX"), (".jsx", "JSX"), (".js", "JS"), (".mjs", "JS"), (".cjs", "JS")];

fn lang_id(ext: &str) -> String {
    let known: &[(&str, &str)] = &[
        (".py", "python"),
        (".rs", "rust"),
        (".go", "go"),
        (".ts", "typescript"),
        (".mts", "typescript"),
        (".cts", "typescript"),
        (".tsx", "typescriptreact"),
        (".js", "javascript"),
        (".mjs", "javascript"),
        (".cjs", "javascript"),
        (".jsx", "javascriptreact"),
        (".c", "c"),
        (".h", "cpp"),
        (".hpp", "cpp"),
        (".cc", "cpp"),
        (".cpp", "cpp"),
        (".cxx", "cpp"),
        (".cs", "csharp"),
        (".java", "java"),
        (".kt", "kotlin"),
        (".kts", "kotlin"),
        (".swift", "swift"),
        (".ex", "elixir"),
        (".exs", "elixir"),
        (".gd", "gdscript"),
        (".rb", "ruby"),
        (".php", "php"),
    ];
    known.iter().find(|(e, _)| *e == ext).map(|(_, l)| l.to_string()).unwrap_or_else(|| ext.trim_start_matches('.').to_string())
}

/// `urllib.parse.quote` (safe "/").
fn quote(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `urllib.parse.unquote(urlparse(uri).path)` of a file:// URI.
fn uri_path(uri: &str) -> String {
    let rest = &uri["file://".len()..];
    let path = match rest.find('/') {
        Some(k) => &rest[k..],
        None => "",
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    let b = path.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(x) = u8::from_str_radix(&path[i + 1..i + 3], 16) {
                out.push(x);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `os.path.join(root, rel)`.
fn join(root: &str, rel: &str) -> String {
    if rel.starts_with('/') {
        rel.to_string()
    } else if root.ends_with('/') {
        format!("{root}{rel}")
    } else {
        format!("{root}/{rel}")
    }
}

/// A client: LSP's textDocument/definition, or tsserver's `definition`.
pub enum Client {
    Lsp {
        conn: Conn,
        root: String,
        next_id: i64,
        /// rust-analyzer's "quiescent", once it reports it
        status: Option<bool>,
        /// $/progress tokens begun and not yet ended
        active: HashSet<String>,
        started: Instant,
        init_options: Value,
    },
    Ts {
        conn: Conn,
        root: String,
        seq: i64,
        answered: bool,
    },
}

impl Client {
    pub fn lsp(argv: &[String], root: &str, env: &[(&str, &str)], init_options: Value) -> std::io::Result<Client> {
        let started = Instant::now();
        let conn = Conn::start(argv, root, env, true)?;
        Ok(Client::Lsp { conn, root: root.into(), next_id: 0, status: None, active: HashSet::new(), started, init_options })
    }

    pub fn ts(argv: &[String], root: &str) -> std::io::Result<Client> {
        Ok(Client::Ts { conn: Conn::start(argv, root, &[], false)?, root: root.into(), seq: 0, answered: false })
    }

    pub fn conn(&mut self) -> &mut Conn {
        match self {
            Client::Lsp { conn, .. } | Client::Ts { conn, .. } => conn,
        }
    }

    pub fn status(&self) -> Option<bool> {
        match self {
            Client::Lsp { status, .. } => *status,
            Client::Ts { .. } => None,
        }
    }

    fn uri(root: &str, rel: &str) -> String {
        format!("file://{}", quote(&join(root, rel)))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<i64, Dead> {
        let Client::Lsp { conn, next_id, .. } = self else { unreachable!() };
        *next_id += 1;
        let id = *next_id;
        conn.send(&frame(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})))?;
        Ok(id)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), Dead> {
        self.conn().send(&frame(&json!({"jsonrpc": "2.0", "method": method, "params": params})))
    }

    fn ts_send(&mut self, command: &str, args: Value) -> Result<i64, Dead> {
        let Client::Ts { conn, seq, .. } = self else { unreachable!() };
        *seq += 1;
        let s = *seq;
        let mut line = serde_json::to_vec(&json!({"seq": s, "type": "request", "command": command, "arguments": args})).unwrap_or_default();
        line.push(b'\n');
        conn.send(&line)?;
        Ok(s)
    }

    pub fn start(&mut self, wait: f64) -> Result<(), Dead> {
        if let Client::Ts { .. } = self {
            self.ts_send("configure", json!({"preferences": {"includePackageJsonAutoImports": "off"}}))?;
            return Ok(());
        }
        let Client::Lsp { root, init_options, .. } = self else { unreachable!() };
        let (root, opts) = (root.clone(), init_options.clone());
        let caps = json!({"textDocument": {"definition": {"linkSupport": true}, "synchronization": {}},
                          "window": {"workDoneProgress": true}, "workspace": {"configuration": true},
                          "experimental": {"serverStatusNotification": true}});
        let rid = self.request(
            "initialize",
            json!({"processId": std::process::id(), "rootUri": Self::uri(&root, ""), "rootPath": root,
                   "workspaceFolders": [{"uri": Self::uri(&root, ""), "name": "root"}],
                   "capabilities": caps, "initializationOptions": opts}),
        )?;
        let deadline = Instant::now() + Duration::from_secs_f64(wait);
        let msg = loop {
            let left = deadline.saturating_duration_since(Instant::now()).as_secs_f64();
            match self.conn().poll(left)? {
                None => return Err(Dead(format!("no answer to initialize in {} s", wait as i64))),
                Some(m) if m.get("id").and_then(Value::as_i64) == Some(rid) => break m,
                Some(m) => {
                    self.answer(&m);
                }
            }
        };
        if let Some(e) = msg.get("error") {
            let m: String = e.get("message").and_then(Value::as_str).unwrap_or("").chars().take(120).collect();
            return Err(Dead(format!("refused to initialize: {m}")));
        }
        self.notify("initialized", json!({}))
    }

    pub fn open(&mut self, rel: &str, text: &str) -> Result<(), Dead> {
        let ext = crate::project::ext_of(rel).to_string();
        match self {
            Client::Ts { root, .. } => {
                let kind = TS_KINDS.iter().find(|(e, _)| *e == ext).map(|(_, k)| *k).unwrap_or("TS");
                let (file, root) = (join(root, rel), root.clone());
                self.ts_send("open", json!({"file": file, "fileContent": text, "scriptKindName": kind, "projectRootPath": root}))?;
                Ok(())
            }
            Client::Lsp { root, .. } => {
                let uri = Self::uri(root, rel);
                self.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": lang_id(&ext), "version": 1, "text": text}}))
            }
        }
    }

    pub fn close(&mut self, rel: &str) -> Result<(), Dead> {
        match self {
            Client::Ts { root, .. } => {
                let file = join(root, rel);
                self.ts_send("close", json!({"file": file})).map(|_| ())
            }
            Client::Lsp { root, .. } => {
                let uri = Self::uri(root, rel);
                self.notify("textDocument/didClose", json!({"textDocument": {"uri": uri}}))
            }
        }
    }

    pub fn ask(&mut self, rel: &str, line0: usize, u16: usize) -> Result<i64, Dead> {
        match self {
            Client::Ts { root, .. } => {
                let file = join(root, rel);
                self.ts_send("definition", json!({"file": file, "line": line0 + 1, "offset": u16 + 1}))
            }
            Client::Lsp { root, .. } => {
                let uri = Self::uri(root, rel);
                self.request("textDocument/definition", json!({"textDocument": {"uri": uri}, "position": {"line": line0, "character": u16}}))
            }
        }
    }

    /// (request id, what it says) for a definition response; None for
    /// anything else (a notification, which may say the server is loaded).
    pub fn answer(&mut self, msg: &Value) -> Option<(i64, Answer)> {
        match self {
            Client::Ts { answered, .. } => {
                if msg.get("type").and_then(Value::as_str) != Some("response") || msg.get("command").and_then(Value::as_str) != Some("definition") {
                    return None;
                }
                *answered = true;
                let seq = msg.get("request_seq").and_then(Value::as_i64).unwrap_or(-1);
                if !msg.get("success").is_some_and(|s| s.as_bool() == Some(true)) {
                    // "No content available": nothing is defined there; anything else is an error
                    let m = match msg.get("message") {
                        Some(Value::String(s)) => s.clone(),
                        Some(v) => v.to_string(),
                        None => "None".into(),
                    };
                    return Some((seq, if m.contains("No content") { Answer::Locs(vec![]) } else { Answer::Error }));
                }
                let locs = msg
                    .get("body")
                    .and_then(Value::as_array)
                    .map(|ds| {
                        ds.iter()
                            .filter(|d| d.get("file").is_some_and(|f| !f.is_null() && f != ""))
                            .map(|d| {
                                let at = |k: &str| d["start"][k].as_i64().unwrap_or(1) - 1;
                                (d["file"].as_str().unwrap_or("").to_string(), at("line").max(0) as usize, at("offset").max(0) as usize)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some((seq, Answer::Locs(locs)))
            }
            Client::Lsp { status, active, .. } => {
                let Some(id) = msg.get("id") else {
                    // rust-analyzer says when its workspace is loaded (experimental/serverStatus);
                    // other servers report work as $/progress begin ... end
                    let p = msg.get("params").cloned().unwrap_or(Value::Null);
                    match msg.get("method").and_then(Value::as_str) {
                        Some("experimental/serverStatus") => *status = Some(p.get("quiescent").is_some_and(truthy)),
                        Some("$/progress") => {
                            let tok = match p.get("token") {
                                Some(Value::String(s)) => s.clone(),
                                Some(v) => v.to_string(),
                                None => "None".into(),
                            };
                            match p.get("value").and_then(|v| v.get("kind")).and_then(Value::as_str) {
                                Some("begin") => {
                                    active.insert(tok);
                                }
                                Some("end") => {
                                    active.remove(&tok);
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                    return None;
                };
                let id = id.as_i64().unwrap_or(-1);
                if let Some(e) = msg.get("error") {
                    let busy = e.get("code").and_then(Value::as_i64).is_some_and(|c| BUSY.contains(&c));
                    return Some((id, if busy { Answer::Retry } else { Answer::Error }));
                }
                let res = match msg.get("result") {
                    None | Some(Value::Null) => vec![],
                    Some(Value::Array(xs)) => xs.clone(),
                    Some(Value::Bool(false)) => vec![],
                    Some(Value::Object(o)) if o.is_empty() => vec![],
                    Some(v) => vec![v.clone()],
                };
                let mut out = Vec::new();
                for loc in res {
                    let uri = loc.get("targetUri").and_then(Value::as_str).filter(|s| !s.is_empty()).or_else(|| loc.get("uri").and_then(Value::as_str)).unwrap_or("");
                    let rng = loc.get("targetSelectionRange").filter(|r| truthy(r)).or_else(|| loc.get("range")).cloned().unwrap_or(Value::Null);
                    if uri.starts_with("file://") {
                        let st = &rng["start"];
                        out.push((uri_path(uri), st["line"].as_u64().unwrap_or(0) as usize, st["character"].as_u64().unwrap_or(0) as usize));
                    }
                }
                Some((id, Answer::Locs(out)))
            }
        }
    }

    /// Done loading the workspace, as far as the server says: its status when
    /// it reports one, else no progress under way after a grace period.
    pub fn loaded(&self) -> bool {
        match self {
            Client::Ts { answered, .. } => *answered, // (it answers only once the file's project is loaded)
            Client::Lsp { status: Some(s), .. } => *s,
            Client::Lsp { active, started, .. } => active.is_empty() && started.elapsed().as_secs_f64() > 10.0,
        }
    }

    pub fn stop(&mut self) {
        match self {
            Client::Ts { .. } => {
                if self.ts_send("exit", json!({})).is_ok() {
                    self.conn().wait_for(3.0);
                }
            }
            Client::Lsp { .. } => {
                if let Ok(rid) = self.request("shutdown", Value::Null) {
                    let deadline = Instant::now() + Duration::from_secs(3);
                    loop {
                        let left = deadline.saturating_duration_since(Instant::now()).as_secs_f64();
                        match self.conn().poll(left) {
                            Ok(Some(m)) if m.get("id").and_then(Value::as_i64) != Some(rid) => continue,
                            _ => break,
                        }
                    }
                }
                let _ = self.notify("exit", Value::Null);
            }
        }
        self.conn().kill();
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// One file's questions: (rel, text, [(line0, u16, key)]).
pub type Batch<K> = Vec<(String, String, Vec<(usize, usize, K)>)>;

/// Pipeline definition requests: open a file, send all its questions, keep
/// up to WINDOW in flight, close a file once all of its are answered. A busy
/// answer is asked again a second later, for up to `timeout`.
pub fn drive<K: Clone + Eq + std::hash::Hash>(
    client: &mut Client,
    files: &Batch<K>,
    timeout: f64,
    first: f64,
    out: &mut HashMap<K, Option<Vec<(String, usize, usize)>>>,
) -> Result<(), Dead> {
    let mut pending: HashMap<i64, (usize, usize)> = HashMap::new(); // id -> (file index, question index)
    let mut left: HashMap<usize, usize> = HashMap::new();
    let mut tries: HashMap<(usize, usize), f64> = HashMap::new();
    let mut later: VecDeque<(Instant, usize, usize)> = VecDeque::new();
    let mut todo = files.iter().enumerate();
    let mut wait = first;
    loop {
        while pending.len() < WINDOW {
            let Some((fi, (rel, text, asks))) = todo.next() else { break };
            client.open(rel, text)?;
            left.insert(fi, asks.len());
            for (qi, q) in asks.iter().enumerate() {
                let id = client.ask(rel, q.0, q.1)?;
                pending.insert(id, (fi, qi));
            }
        }
        while later.front().is_some_and(|x| x.0 <= Instant::now()) {
            let (_, fi, qi) = later.pop_front().unwrap();
            let q = &files[fi].2[qi];
            let id = client.ask(&files[fi].0, q.0, q.1)?;
            pending.insert(id, (fi, qi));
        }
        if pending.is_empty() && later.is_empty() {
            return Ok(());
        }
        let quiet = Instant::now();
        let t = match later.front() {
            Some(x) => wait.min(x.0.saturating_duration_since(quiet).as_secs_f64()),
            None => wait,
        };
        let Some(msg) = client.conn().poll(t)? else {
            if !later.is_empty() && quiet.elapsed().as_secs_f64() < wait {
                continue; // (only waited for a retry to come due)
            }
            return Err(Dead(format!("no answer in {} s", wait as i64)));
        };
        let Some((id, got)) = client.answer(&msg) else { continue };
        let Some((fi, qi)) = pending.remove(&id) else { continue };
        wait = timeout; // (the first answer may wait on the workspace loading)
        let n = tries.entry((fi, qi)).or_insert(0.0);
        if got == Answer::Retry && *n < timeout {
            *n += 1.0;
            later.push_back((Instant::now() + Duration::from_secs(1), fi, qi));
            continue;
        }
        out.insert(
            files[fi].2[qi].2.clone(),
            match got {
                Answer::Locs(l) => Some(l),
                _ => None,
            },
        );
        let l = left.get_mut(&fi).unwrap();
        *l -= 1;
        if *l == 0 {
            client.close(&files[fi].0)?;
        }
    }
}

/// Read what the server says about its progress for a while.
pub fn listen(client: &mut Client, secs: f64) -> Result<(), Dead> {
    let end = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < end {
        let left = end.saturating_duration_since(Instant::now()).as_secs_f64();
        match client.conn().poll(left)? {
            Some(m) => {
                client.answer(&m);
            }
            None => break,
        }
    }
    Ok(())
}

/// Wait for the server to know the workspace: ask calls the static resolver
/// found for certain until one is answered (rust-analyzer answers nothing
/// until it has loaded the crates). Once the server says it is done loading
/// and a round still finds nothing, the probes are calls it can't follow.
pub fn ready<K: Clone + Eq + std::hash::Hash>(client: &mut Client, probes: &Batch<K>, wait: f64) -> Result<bool, Dead> {
    if probes.is_empty() {
        return Ok(false);
    }
    let deadline = Instant::now() + Duration::from_secs_f64(wait);
    loop {
        let done = client.loaded();
        let left = deadline.saturating_duration_since(Instant::now()).as_secs_f64().max(1.0);
        let mut got = HashMap::new();
        drive(client, probes, left, left, &mut got)?;
        if got.values().any(|v| v.as_ref().is_some_and(|l| !l.is_empty())) {
            break;
        }
        if done || Instant::now() >= deadline {
            return Ok(false);
        }
        listen(client, 1.0)?;
    }
    // one crate answering is not all of them (nor std): a server that reports
    // its status (rust-analyzer) is asked only once it says it is done
    while client.status() == Some(false) && Instant::now() < deadline {
        listen(client, 1.0)?;
    }
    Ok(true)
}
