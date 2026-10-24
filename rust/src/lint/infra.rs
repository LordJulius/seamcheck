//! Lints across deploy scripts, containers and services (lint_infra.py):
//! the seams between a program and the machines it runs on.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use serde_json::{Map, Value};

use super::{line, opt_list, opt_str, rxs, Hits};
use crate::project::Project;
use crate::pyre::{repr, strip, Rx};
use crate::rules::{compile, RuleErr};

type Res<T> = Result<T, RuleErr>;

fn shell(rel: &str, raw: &str) -> bool {
    if rel.ends_with(".sh") || rel.ends_with(".bash") {
        return true;
    }
    let first = raw.split('\n').next().unwrap_or("");
    first.starts_with("#!") && rxs(64, r"\b(ba|z|da)?sh\b").is_match(first)
}

fn norm_path(p: &str) -> String {
    strip(p).trim_matches('"').trim_matches('\'').to_string()
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn dirname(p: &str) -> &str {
    match p.rfind('/') {
        Some(k) => &p[..k],
        None => "",
    }
}

// --- shell-reexec-watch -----------------------------------------------------------

pub fn shell_reexec_watch(rel: &str, raw: &str, _code: &str, _o: &Map<String, Value>, _p: &Project) -> Res<Hits> {
    if !shell(rel, raw) || !rxs(65, r#"\bexec\s+("?\$(?:self|0|\{0\})"?|"?\$\(readlink -f "?\$0"?\)"?)"#).is_match(raw) {
        return Ok(vec![]);
    }
    let stamp_lines = rxs(66, r"(?m)^.*\b(?:stat|sha1sum|sha256sum|md5sum|cksum)\b.*$").find_iter(raw);
    if stamp_lines.is_empty() {
        return Ok(vec![]);
    }
    let watched_text = stamp_lines.iter().map(|m| m.group0()).collect::<Vec<_>>().join("\n");
    let watched: BTreeSet<String> = rxs(67, r#"("\$[^"]+"|\$\{?\w+\}?/[\w.${}:/-]+)"#)
        .findall(&watched_text)
        .into_iter()
        .map(norm_path)
        .collect();
    let mut out = Vec::new();
    // parity: Python keeps `sourced` in a set (hash order); first-seen order here
    let mut sourced: Vec<String> = Vec::new();
    for m in rxs(68, r#"(?m)(?:^|&&|\|\||;|\bthen|\bdo)\s*(?:\.|source)\s+("[^"]+"|'[^']+'|[^\s;&|]+)"#).find_iter(raw) {
        let p = norm_path(m.s(1));
        if !(p.starts_with('$') || p.starts_with('/')) || p.starts_with("/dev/") || p.starts_with("/etc/") {
            continue;
        }
        if let Some(var) = rxs(69, r"\$\{?(\w+)\}?$").match_start(&p) {
            let pat = format!(r#"(?m)^\s*(?:readonly\s+|export\s+)?{}=["']?(?:\$\{{\w+:-)?/etc/"#, var.s(1));
            if compile(&pat)?.is_match(raw) {
                continue;
            }
        }
        if !sourced.contains(&p) {
            sourced.push(p.clone());
        }
        if !watched.contains(&p) && !watched_text.contains(p.as_str()) {
            out.push((
                line(raw, m.start()),
                format!(
                    "sources {p} but the re-exec stamp doesn't watch it: a deploy that only changes {} never reaches the running process. Add it to the stamp's stat list.",
                    basename(&p)
                ),
            ));
        }
    }
    for p in &watched {
        let base = basename(p);
        if !(base.ends_with(".sh") || base.ends_with(".env") || base.ends_with(".conf")) || sourced.contains(p) {
            continue;
        }
        let stem = base.split('$').next().unwrap_or("");
        let twins: Vec<&String> = if stem.is_empty() {
            vec![]
        } else {
            sourced.iter().filter(|s| basename(s).starts_with(stem) && *s != p).collect()
        };
        if let Some(t) = twins.first() {
            let ln = stamp_lines.iter().find(|m| m.group0().contains(p.as_str())).map(|m| line(raw, m.start())).unwrap_or(1);
            out.push((
                ln,
                format!(
                    "the stamp watches {p} but the script sources {t}: the watched path is not the file that runs (it may not exist for every node). Use the same expression in both places."
                ),
            ));
        }
    }
    Ok(out)
}

// --- shell-name-domain ------------------------------------------------------------

pub fn shell_name_domain(rel: &str, raw: &str, _code: &str, o: &Map<String, Value>, _p: &Project) -> Res<Hits> {
    if !shell(rel, raw) {
        return Ok(vec![]);
    }
    let names = compile(&opt_str(o, "names", r"(?i)^(?:node|name|peer|host|target|to|who|other|partner|n|nd)$"))?;
    let mut converted: HashSet<String> =
        rxs(70, r"\$\{(\w+)//-/_\}").findall(raw).into_iter().map(String::from).collect();
    for w in rxs(71, r#"(\w+)=\$\(\s*(?:echo|printf[^|]*)\s+"?\$\{?\w+\}?"?\s*\|\s*tr\s+['"]?-['"]?\s+['"]?_"#).findall(raw) {
        converted.insert(w.to_string());
    }
    let mut out = Vec::new();
    for (i, ln) in raw.split('\n').enumerate() {
        let i = i + 1;
        let s = if !ln.trim_start().starts_with("#!") { ln.split('#').next().unwrap_or("") } else { "" };
        for m in rxs(72, r"\\\$\{\$\{?(\w+)\}?_\w+(?::?-[^}]*)?\}").find_iter(s) {
            let v = m.s(1);
            if names.is_match(v) && !converted.contains(v) {
                out.push((
                    i,
                    format!(
                        "${{{v}}} becomes part of a shell variable name: a name with '-' (pylon-primos) makes an invalid variable and the lookup comes back empty. Use ${{{v}//-/_}} for the key."
                    ),
                ));
            }
        }
        for m in rxs(73, r#"\btr\s+-d?c?d?\s+(['"])([^'"]+)\1"#).find_iter(s) {
            let (flags, chars) = (m.group0(), m.s(2));
            if (flags.contains("-dc") || flags.contains("-cd"))
                && rxs(74, r"(?i)node|peer|handoff|request|target|host|\bto\b|\bwho\b").is_match(s)
                && !rxs(75, r"(^|[^a-z])-|-$|\\-").is_match(chars)
                && rxs(76, r"a-z").is_match(chars)
            {
                out.push((
                    i,
                    format!(
                        "`tr -dc '{chars}'` strips '-' from what it filters: a node named pylon-primos reads as pylonprimos and matches nothing. Allow '-' in the set."
                    ),
                ));
            }
        }
        for m in rxs(77, r"\b[a-z]+_\$\{?(\w+)\}?\b").find_iter(s) {
            let v = m.s(1);
            if names.is_match(v)
                && !converted.contains(v)
                && rxs(78, r"(?i)slot|replication|_slot|create_physical|primary_slot").is_match(s)
            {
                out.push((
                    i,
                    format!(
                        "identifier built from ${v}: a name with '-' is not a valid Postgres/shell identifier. Convert with ${{{v}//-/_}}."
                    ),
                ));
            }
        }
    }
    Ok(out)
}

// --- home-paths-mounted -----------------------------------------------------------

fn compose_targets(p: &Project, pattern: &str, service: &str) -> Arc<Option<Vec<String>>> {
    super::shared(p, &format!("compose\0{pattern}\0{service}"), || {
        let mut targets: Option<Vec<String>> = None;
        let svc = Rx::new(&format!(r"(?m)^(\s+){}:\s*$", regex::escape(service))).ok();
        for rel in crate::walk::list_files(&p.root, &[pattern.to_string()], &[]) {
            let Some(text) = crate::project::read_text(&p.path(&rel)) else { continue };
            let Some(m) = svc.as_ref().and_then(|r| r.search(&text)) else { continue };
            let ind = m.s(1).chars().count();
            let mut block: Vec<&str> = Vec::new();
            for ln in text[m.end()..].split('\n').skip(1) {
                let lead = ln.chars().count() - ln.trim_start().chars().count();
                if !strip(ln).is_empty() && lead <= ind {
                    break;
                }
                block.push(ln);
            }
            let t = targets.get_or_insert_with(Vec::new);
            let mut in_vol = false;
            for ln in block {
                if rxs(79, r"^\s+volumes:\s*$").match_start(ln).is_some() {
                    in_vol = true;
                    continue;
                }
                if in_vol {
                    if let Some(vm) = rxs(80, r#"^\s+-\s+["']?([^"':]+):([^"':]+)"#).match_start(ln) {
                        t.push(vm.s(2).trim_end_matches('/').to_string());
                    } else if !strip(ln).is_empty() && !strip(ln).starts_with('#') {
                        in_vol = false;
                    }
                }
            }
        }
        targets
    })
}

pub fn home_paths_mounted(rel: &str, raw: &str, _code: &str, o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    if !(rel.ends_with(".ex") || rel.ends_with(".exs")) {
        return Ok(vec![]);
    }
    let service = opt_str(o, "service", "hub");
    let targets = compose_targets(p, &opt_str(o, "compose", "**/compose.yml"), &service);
    let Some(targets) = targets.as_ref() else { return Ok(vec![]) };
    let mut homes: BTreeSet<String> = BTreeSet::new();
    for t in targets {
        if let Some(m) = rxs(81, r"^(/home/[^/]+|/root)(?:/|$)").match_start(t) {
            homes.insert(m.s(1).to_string());
        }
    }
    let home = match o.get("home").and_then(|h| h.as_str()).filter(|h| !h.is_empty()) {
        Some(h) => h.to_string(),
        None if homes.len() == 1 => homes.into_iter().next().unwrap(),
        None => std::env::var("HOME").unwrap_or_else(|_| "~".into()),
    };
    let home = home.trim_end_matches('/').to_string();
    let allow: Vec<Arc<Rx>> = opt_list(o, "allow", &[]).iter().map(|a| compile(a)).collect::<Res<_>>()?;
    let mut hits: Vec<(usize, String)> =
        rxs(82, r##""~/([^"#{}]+)""##).find_iter(raw).iter().map(|m| (m.start(), m.s(1).to_string())).collect();
    for m in rxs(83, r#"System\.user_home!?\(\)\s*,\s*((?:"[^"]+"\s*,?\s*)+)"#).find_iter(raw) {
        let parts = rxs(84, r#""([^"]+)""#).findall(m.s(1));
        hits.push((m.start(), parts.join("/")));
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (pos, sub) in hits {
        let sub = sub.trim_matches('/').to_string();
        if sub.is_empty() || seen.contains(&sub) || allow.iter().any(|a| a.is_match(&sub)) {
            continue;
        }
        seen.insert(sub.clone());
        let full = format!("{home}/{sub}");
        if targets.iter().any(|t| full == *t || full.starts_with(&format!("{t}/")) || t.starts_with(&format!("{full}/"))) {
            continue;
        }
        out.push((
            line(raw, pos),
            format!(
                "~/{sub} is not under any volume of the `{service}` container: on a node that runs the hub in a container it is missing, and anything written there is lost on recreate. Mount it (and sync it between nodes), or list it in `allow` if it is host-only by design."
            ),
        ));
    }
    Ok(out)
}

// --- stale-module-ref -------------------------------------------------------------

struct StaleIndex {
    prefixes: HashSet<String>,
    rx: Option<Rx>,
}

pub fn stale_module_ref(rel: &str, raw: &str, code: &str, o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    if !(rel.ends_with(".ex") || rel.ends_with(".exs")) {
        return Ok(vec![]);
    }
    let ns = opt_list(o, "namespaces", &[]);
    // (outside the build-once guard: module_names parses in parallel)
    let names = p.module_names();
    let idx = super::shared(p, &format!("stale-index\0{}", ns.join("\0")), || {
        let roots: Vec<String> = if !ns.is_empty() {
            ns.clone()
        } else {
            let mut tops: Vec<(String, usize)> = Vec::new();
            for n in names {
                let top = n.split('.').next().unwrap_or("");
                match tops.iter_mut().find(|(t, _)| t == top) {
                    Some(slot) => slot.1 += 1,
                    None => tops.push((top.to_string(), 1)),
                }
            }
            tops.into_iter().filter(|(_, c)| *c >= 5).map(|(t, _)| t).collect()
        };
        let mut prefixes = HashSet::new();
        for n in names {
            let parts: Vec<&str> = n.split('.').collect();
            for k in 1..=parts.len() {
                prefixes.insert(parts[..k].join("."));
            }
        }
        let rx = if roots.is_empty() {
            None
        } else {
            let alt: Vec<String> = roots.iter().map(|r| regex::escape(r)).collect();
            Rx::new(&format!(r"(?<![\w.])((?:{})(?:\.[A-Z]\w*)+)", alt.join("|"))).ok()
        };
        StaleIndex { prefixes, rx }
    });
    let Some(rx) = &idx.rx else { return Ok(vec![]) };
    let hist = rxs(85, r"(?i)\b(removed|retired|replaced|deleted|gone|formerly|renamed|was|no longer|used to)\b");
    let mut out = Vec::new();
    for m in rx.find_iter(raw) {
        if m.start() < code.len() && code.get(m.start()..m.end()) == Some(m.group0()) {
            continue;
        }
        let r = m.s(1);
        if idx.prefixes.contains(r) {
            continue;
        }
        let ls = raw[..m.start()].rfind('\n').map(|k| k + 1).unwrap_or(0);
        let le = raw[m.end()..].find('\n').map(|k| m.end() + k).unwrap_or(raw.len());
        if hist.is_match(&raw[ls..le]) {
            continue;
        }
        out.push((
            line(raw, m.start()),
            format!(
                "{r} is named in a doc or comment but no such module exists any more: rewrite the text for what replaced it (a stale doc misleads the next reader and every agent)."
            ),
        ));
    }
    Ok(out)
}

// --- lockfile-sync ----------------------------------------------------------------

/// Python `repr()` of a JSON value (None for a missing key).
fn py_repr(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => repr(s),
        Some(Value::Bool(b)) => if *b { "True" } else { "False" }.into(),
        Some(other) => other.to_string(),
    }
}

pub fn lockfile_sync(rel: &str, raw: &str, _code: &str, _o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    if basename(rel) != "package-lock.json" {
        return Ok(vec![]);
    }
    let Ok(lock) = serde_json::from_str::<Value>(raw) else { return Ok(vec![]) };
    let pj = if dirname(rel).is_empty() { "package.json".to_string() } else { format!("{}/package.json", dirname(rel)) };
    let Some(pkg) = crate::project::read_text(&p.path(&pj)).and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        return Ok(vec![]);
    };
    let obj = |v: Option<&Value>| -> Option<Map<String, Value>> {
        match v {
            Some(Value::Object(m)) if !m.is_empty() => Some(m.clone()),
            _ => None,
        }
    };
    let root = obj(lock.get("packages")).and_then(|pk| obj(pk.get(""))).unwrap_or_default();
    let mut out = Vec::new();
    for sect in ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"] {
        let want = obj(pkg.get(sect)).unwrap_or_default();
        let have = obj(root.get(sect)).unwrap_or_default();
        for (name, rng) in &want {
            if have.get(name) != Some(rng) {
                out.push((
                    1,
                    format!(
                        "package.json {sect}.{name} is {} but package-lock.json records {}: `npm ci` refuses on a fresh node. Run `npm install` and commit the lock.",
                        py_repr(Some(rng)),
                        py_repr(have.get(name))
                    ),
                ));
            }
        }
        // parity: Python walks `set(have) - set(want)` (hash order); lock order here
        for name in have.keys() {
            if !want.contains_key(name) {
                out.push((
                    1,
                    format!(
                        "package-lock.json still records {sect}.{name}, which package.json no longer declares: run `npm install` and commit the lock."
                    ),
                ));
            }
        }
    }
    Ok(out)
}

// --- path-unit-oneshot ------------------------------------------------------------

pub fn path_unit_oneshot(rel: &str, raw: &str, _code: &str, o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    if !rel.ends_with(".path") {
        return Ok(vec![]);
    }
    let svc = match rxs(86, r"(?m)^\s*Unit\s*=\s*(\S+)").search(raw) {
        Some(m) => m.s(1).to_string(),
        None => {
            let b = basename(rel);
            format!("{}.service", &b[..b.len() - 5])
        }
    };
    let path = if dirname(rel).is_empty() { svc.clone() } else { format!("{}/{svc}", dirname(rel)) };
    let Some(body) = std::fs::read_to_string(p.path(&path)).ok() else { return Ok(vec![]) };
    if !rxs(87, r"(?m)^\s*Type\s*=\s*oneshot\b").is_match(&body) {
        return Ok(vec![]);
    }
    if compile(&opt_str(o, "settles", r"(?i)settle|debounce|re-?read|until .* stable|converge"))?.is_match(&body) {
        return Ok(vec![]);
    }
    let ln = rxs(88, r"(?m)^\s*Path(?:Changed|Modified)\s*=").search(raw).map(|m| line(raw, m.start())).unwrap_or(1);
    Ok(vec![(
        ln,
        format!(
            "triggers {svc} (Type=oneshot): a change that lands while it runs is dropped, so rapid writes end on the first one. Have the service re-read until the input is stable (and say so in a comment: settle/re-read)."
        ),
    )])
}

// --- css-braces -------------------------------------------------------------------

pub fn css_braces(rel: &str, raw: &str, _code: &str, _o: &Map<String, Value>, _p: &Project) -> Res<Hits> {
    if !(rel.ends_with(".css") || rel.ends_with(".scss")) {
        return Ok(vec![]);
    }
    let blankish = |s: &str, keep_nl: bool| -> String {
        s.chars().map(|c| if keep_nl && c == '\n' { '\n' } else { ' ' }).collect()
    };
    let text = rxs(89, r"(?s)/\*.*?\*/").sub_with(raw, |m| blankish(m.group0(), true));
    let text = rxs(90, r#""(?:\\.|[^"\\\n])*"|'(?:\\.|[^'\\\n])*'"#).sub_with(&text, |m| blankish(m.group0(), false));
    let mut depth = 0i64;
    let mut opens: Vec<usize> = Vec::new();
    for (i, ch) in text.bytes().enumerate() {
        if ch == b'{' {
            depth += 1;
            opens.push(i);
        } else if ch == b'}' {
            depth -= 1;
            if depth < 0 {
                return Ok(vec![(line(&text, i), "a `}` closes nothing: a block above lost its opening brace (a bad merge?)".into())]);
            }
            opens.pop();
        }
    }
    if depth != 0 {
        return Ok(vec![(
            line(&text, *opens.last().unwrap()),
            format!(
                "{depth} `{{` never closed (from this block on): a merge dropped a closing brace and the rest of the sheet is swallowed into this rule"
            ),
        )]);
    }
    Ok(vec![])
}

// --- toolchain-pin ------------------------------------------------------------------

pub fn toolchain_pin(_rel: &str, raw: &str, code: &str, _o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    let pins = super::shared(p, "toolchain-pins", || {
        let ex: Vec<String> = crate::walk::DEFAULT_EXCLUDE.iter().map(|s| s.to_string()).collect();
        let mut pins: Vec<(String, String)> = Vec::new();
        for t in crate::walk::list_files(&p.root, &["**/rust-toolchain.toml".into(), "**/rust-toolchain".into()], &ex) {
            let b = basename(&t);
            if b != "rust-toolchain.toml" && b != "rust-toolchain" {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(p.path(&t)) else { continue };
            let ch = rxs(91, r#"channel\s*=\s*"([^"]+)""#)
                .search(&body)
                .or_else(|| rxs(92, r"\s*(\S+)\s*$").match_start(&body))
                .map(|m| m.s(1).to_string());
            if let Some(c) = ch {
                let d = dirname(&t).to_string();
                match pins.iter_mut().find(|(k, _)| *k == d) {
                    Some(slot) => slot.1 = c,
                    None => pins.push((d, c)),
                }
            }
        }
        pins
    });
    if pins.is_empty() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for m in rxs(93, r"rustup\s+run\s+([\w.\-]+)|\bcargo\s+\+([\w.\-]+)").find_iter(code) {
        let used = m.get(1).or(m.get(2)).unwrap_or("");
        if used.starts_with('$') {
            continue;
        }
        let named: Vec<&(String, String)> = pins.iter().filter(|(d, _)| !d.is_empty() && raw.contains(d.as_str())).collect();
        let chosen: Vec<&(String, String)> = if !named.is_empty() {
            named
        } else if pins.len() == 1 {
            pins.iter().collect()
        } else {
            vec![]
        };
        let want: BTreeSet<&str> = chosen.iter().map(|(_, c)| c.as_str()).collect();
        if !want.is_empty() && !want.contains(used) {
            out.push((
                line(code, m.start()),
                format!(
                    "runs toolchain {} but {} is pinned in rust-toolchain.toml: two names for one toolchain, and a build node with only the pinned one fails",
                    repr(used),
                    repr(want.iter().next().unwrap())
                ),
            ));
        }
    }
    Ok(out)
}

// --- test-reads-home ----------------------------------------------------------------

pub fn test_reads_home(_rel: &str, raw: &str, _code: &str, o: &Map<String, Value>, _p: &Project) -> Res<Hits> {
    let pat = compile(&opt_str(
        o,
        "pattern",
        r#"Path\.expand\(\s*"~|System\.user_home|os\.homedir\(\)|expanduser\(|getenv\(\s*["']HOME["']\)|System\.get_env\(\s*"HOME""#,
    ))?;
    let mut out = Vec::new();
    for m in pat.find_iter(raw) {
        let ls = raw[..m.start()].rfind('\n').map(|k| k + 1).unwrap_or(0);
        let before = raw[ls..m.start()].trim_start();
        if before.starts_with('#') || before.starts_with("//") {
            continue;
        }
        out.push((
            line(raw, m.start()),
            "a test reading the home directory passes only on the machine that has those files: put the fixture in the repo or a temp dir".into(),
        ));
    }
    Ok(out)
}

// --- shell-pkill-self -------------------------------------------------------------

fn pkill() -> &'static Rx {
    rxs(94, r#"\bp(?:kill|grep)\s+(?:-\w+\s+)*-\w*f\w*\s+(?:\\?(["'])(.+?)\\?\1|([^\s;|&"']+))"#)
}

fn first_rest(pat: &str) -> (String, String) {
    let mut c = pat.chars();
    let f = c.next().map(|x| x.to_string()).unwrap_or_default();
    (f, c.collect())
}

pub fn shell_pkill_self(rel: &str, raw: &str, _code: &str, _o: &Map<String, Value>, _p: &Project) -> Res<Hits> {
    if !shell(rel, raw) {
        return Ok(vec![]);
    }
    let mut out: BTreeSet<(usize, String)> = BTreeSet::new();
    let own = basename(rel);
    for m in rxs(95, r#"(?s)\b(?:ssh\b[^\n"'|;&]*?|(?:ba|z|da)?sh\s+-\w*c\s+)(["'])(.*?)(?<!\\)\1"#).find_iter(raw) {
        let (g2s, _) = m.span(2).unwrap();
        for k in pkill().find_iter(m.s(2)) {
            let pat = k.get(2).or(k.get(3)).unwrap_or("");
            if !(pat.starts_with('[') || pat.starts_with('$')) {
                let (f, r) = first_rest(pat);
                out.insert((
                    line(raw, g2s + k.start()),
                    format!(
                        "pkill/pgrep -f {} runs inside a command string that contains {}: it matches the shell running it, which dies before the rest runs. Write the pattern as '[{f}]{r}', or use a pid file.",
                        repr(pat),
                        repr(pat)
                    ),
                ));
            }
        }
    }
    for k in pkill().find_iter(raw) {
        let pat = k.get(2).or(k.get(3)).unwrap_or("");
        if pat.chars().count() >= 3 && !(pat.starts_with('[') || pat.starts_with('$')) && own.contains(pat) {
            let (f, r) = first_rest(pat);
            out.insert((
                line(raw, k.start()),
                format!(
                    "pkill/pgrep -f {} matches this script's own command line (bash {own}): it kills itself. Write '[{f}]{r}'.",
                    repr(pat)
                ),
            ));
        }
    }
    Ok(out.into_iter().collect())
}
