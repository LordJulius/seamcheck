//! Which server for which file (precise.py's `DEFAULTS`, `servers`, `_which`,
//! `_client`, `_rust_project`).

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde_json::{json, Value};

use super::lsp::Client;
use crate::config::Config;

const TS: &[&str] = &[".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"];

fn defaults() -> IndexMap<String, Vec<String>> {
    let mut d: IndexMap<String, Vec<String>> = IndexMap::new();
    d.insert(".py".into(), vec!["pyright-langserver".into(), "--stdio".into()]);
    d.insert(".rs".into(), vec!["rust-analyzer".into()]);
    d.insert(".go".into(), vec!["gopls".into()]);
    for e in TS {
        d.insert(e.to_string(), vec!["tsserver".into()]);
    }
    d
}

fn executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.is_file() && std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

/// `shutil.which`.
fn on_path(exe: &str) -> Option<PathBuf> {
    if exe.contains('/') {
        let p = PathBuf::from(exe);
        return executable(&p).then_some(p);
    }
    std::env::var_os("PATH")
        .map(|v| std::env::split_paths(&v).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.join(exe))
        .find(|p| executable(p))
}

fn rustup_has(exe: &str) -> bool {
    std::process::Command::new("rustup").args(["which", exe]).output().is_ok_and(|o| o.status.success())
}

/// The command for `exe` if it is installed: on PATH (not a rustup proxy for
/// a component that isn't installed), gopls in GOBIN / GOPATH/bin, tsserver
/// as node running the typescript package's own lib/tsserver.js.
pub fn which(exe: &str) -> Option<Vec<String>> {
    let hit = on_path(exe);
    if let Some(h) = &hit {
        let real = std::fs::canonicalize(h).unwrap_or(h.clone());
        if real.file_name().is_some_and(|n| n.to_string_lossy().starts_with("rustup")) && !rustup_has(exe) {
            return None; // rustup's proxy for a component that isn't installed (GitHub's runners have one)
        }
    }
    if hit.is_some() || exe.contains('/') {
        return hit.map(|h| vec![h.to_string_lossy().into_owned()]);
    }
    if exe == "gopls" {
        // `go install` puts it in GOPATH/bin, often not on PATH
        let gopath = std::env::var("GOPATH").unwrap_or_else(|_| format!("{}/go", std::env::var("HOME").unwrap_or_default()));
        for d in [std::env::var("GOBIN").unwrap_or_default(), format!("{gopath}/bin")] {
            if !d.is_empty() && executable(&Path::new(&d).join(exe)) {
                return Some(vec![format!("{d}/{exe}")]);
            }
        }
    }
    if exe == "tsserver" {
        if let Some(tsc) = on_path("tsc") {
            let real = std::fs::canonicalize(&tsc).unwrap_or(tsc);
            let js = real.parent().and_then(Path::parent).map(|d| d.join("lib").join("tsserver.js"));
            if let (Some(js), Some(node)) = (js.filter(|j| j.exists()), on_path("node")) {
                return Some(vec![node.to_string_lossy().into_owned(), js.to_string_lossy().into_owned()]);
            }
        }
    }
    None
}

/// ext -> the command to run, for each server that is installed, and a
/// warning for each one the project configured that isn't. A built-in
/// default that isn't installed is simply not used.
pub fn servers(config: &Config) -> (IndexMap<String, Vec<String>>, Vec<String>) {
    let conf: IndexMap<String, Value> =
        config.table("precise").get("servers").and_then(Value::as_object).map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
    let mut all: IndexMap<String, Value> = defaults().into_iter().map(|(k, v)| (k, json!(v))).collect();
    for (k, v) in &conf {
        all.insert(k.clone(), v.clone());
    }
    let (mut out, mut warn) = (IndexMap::new(), Vec::new());
    for (ext, argv) in all {
        let argv: Vec<String> = match argv {
            Value::String(s) if !s.is_empty() => vec![s],
            Value::Array(xs) if !xs.is_empty() => xs.iter().map(crate::config::py_str).collect(),
            _ => continue, // `".js" = []`: that language stays static
        };
        match which(&argv[0]) {
            Some(mut exe) => {
                exe.extend(argv[1..].iter().cloned());
                out.insert(ext, exe);
            }
            None if conf.contains_key(&ext) => warn.push(format!("precise: {} ({ext}) is not installed", argv[0])),
            None => {}
        }
    }
    (out, warn)
}

fn base(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

fn is_ts(argv: &[String]) -> bool {
    argv.iter().take(2).any(|a| base(a).starts_with("tsserver"))
}

/// The name a server goes by in warnings and stats.
pub fn name(argv: &[String]) -> String {
    if is_ts(argv) {
        "tsserver".into()
    } else {
        base(&argv[0]).to_string()
    }
}

/// A client for argv. The built-in servers get the options that keep them
/// from fetching, building or checking anything: precise mode only asks
/// where names are defined.
pub fn client(argv: &[String], root: &str) -> std::io::Result<Client> {
    let nm = name(argv);
    let default = defaults().values().any(|d| name(d) == nm);
    if is_ts(argv) {
        let mut a = argv.to_vec();
        if default {
            a.extend(["--disableAutomaticTypingAcquisition".to_string(), "--suppressDiagnosticEvents".to_string()]);
        }
        return Client::ts(&a, root);
    }
    if default && nm == "gopls" {
        return Client::lsp(argv, root, &[("GOPROXY", "off")], Value::Null); // modules already downloaded only
    }
    if default && nm == "rust-analyzer" {
        let mut opts = json!({"cargo": {"buildScripts": {"enable": false}, "extraEnv": {"CARGO_NET_OFFLINE": "true"}},
                              "procMacro": {"enable": false}, "checkOnSave": false, "cachePriming": {"enable": false},
                              "diagnostics": {"enable": false}});
        if let Some(linked) = rust_project(root) {
            opts["linkedProjects"] = json!([linked]);
        }
        return Client::lsp(argv, root, &[("CARGO_NET_OFFLINE", "true")], opts);
    }
    Client::lsp(argv, root, &[], Value::Null)
}

fn cargo_meta(root: &str, extra: &[&str]) -> Option<Value> {
    let mut c = std::process::Command::new("cargo");
    c.args(["metadata", "--format-version", "1", "--offline"]).args(extra).current_dir(root).env("CARGO_NET_OFFLINE", "true");
    let o = c.output().ok()?;
    if !o.status.success() {
        return None;
    }
    serde_json::from_slice(&o.stdout).ok()
}

/// When cargo can't resolve the workspace offline (its dependencies were
/// never fetched), rust-analyzer would load nothing. The workspace's own
/// crates, described from `cargo metadata --no-deps`, still let it follow
/// every call between them (and into std): a rust-project.json, inline.
fn rust_project(root: &str) -> Option<Value> {
    if !Path::new(root).join("Cargo.toml").exists() || cargo_meta(root, &[]).is_some() {
        return None; // resolvable (or not cargo at all): rust-analyzer loads it the usual way
    }
    let m = cargo_meta(root, &["--no-deps"])?;
    let mut crates: Vec<(&Value, &Value)> = Vec::new();
    let mut lib: IndexMap<String, usize> = IndexMap::new();
    let kinds = |t: &Value| -> Vec<String> { t["kind"].as_array().map(|a| a.iter().filter_map(|k| k.as_str().map(String::from)).collect()).unwrap_or_default() };
    let empty = vec![];
    for pkg in m["packages"].as_array().unwrap_or(&empty) {
        for t in pkg["targets"].as_array().unwrap_or(&empty) {
            let k = kinds(t);
            if k.iter().any(|x| matches!(x.as_str(), "lib" | "rlib" | "proc-macro" | "bin")) {
                if !k.iter().any(|x| x == "bin") {
                    lib.insert(pkg["name"].as_str().unwrap_or("").to_string(), crates.len());
                }
                crates.push((pkg, t));
            }
        }
    }
    let mut out = Vec::new();
    for (pkg, t) in &crates {
        let pname = pkg["name"].as_str().unwrap_or("");
        let mut deps: Vec<Value> = pkg["dependencies"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter_map(|d| {
                let dn = d["name"].as_str()?;
                let k = *lib.get(dn)?;
                (k != out.len()).then(|| {
                    let nm = d.get("rename").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(dn);
                    json!({"crate": k, "name": nm.replace('-', "_")})
                })
            })
            .collect();
        if kinds(t).iter().any(|x| x == "bin") {
            if let Some(k) = lib.get(pname) {
                deps.push(json!({"crate": k, "name": pname.replace('-', "_")}));
            }
        }
        let mut cfg = vec![json!("test"), json!("debug_assertions")];
        if let Some(fs) = pkg["features"].as_object() {
            cfg.extend(fs.keys().map(|f| json!(format!("feature=\"{f}\""))));
        }
        out.push(json!({"display_name": t["name"].as_str().unwrap_or("").replace('-', "_"), "root_module": t["src_path"],
                        "edition": t.get("edition").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("2021"),
                        "deps": deps, "is_workspace_member": true, "cfg": cfg}));
    }
    let mut proj = json!({"crates": out});
    if let Ok(o) = std::process::Command::new("rustc").args(["--print", "sysroot"]).output() {
        let sysroot = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if !sysroot.is_empty() {
            // (std's sources come with rustup's rust-src component)
            let src = Path::new(&sysroot).join("lib/rustlib/src/rust/library");
            proj["sysroot"] = json!(sysroot);
            if src.is_dir() {
                proj["sysroot_src"] = json!(src.to_string_lossy());
            }
        }
    }
    Some(proj)
}
