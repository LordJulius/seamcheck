//! One verify run: doctor, launch, the steps (each an `act` line and a
//! `state` line in the runtime protocol), the app's log, cleanup, then the
//! judge. Verify's own expectations are runtime rules too (`verify.*`), so
//! one checker gives the verdict and `seamcheck trace` can re-judge a run.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use regex::Regex;
use serde_json::{json, Value};

use super::adb::{expand, Adb};
use super::check;
use super::config::{feature_hash, Feature, Step, Target};
use super::driver::Driver;
use super::evidence;
use super::tree::Screen;
use crate::runtime::{format_violation, Checker, Violation};

/// What verify itself expects of every run, as runtime rules.
fn builtin_rules() -> Value {
    json!({
        "invariant": [
            {"id": "verify.in-front", "assert": "front", "why": "the app left the foreground (a crash, another app, a system dialog)"}
        ],
        "contract": [
            {"id": "verify.step-ran", "act": ["*"], "assert": "ok", "why": "a step could not run: its control was not on screen, or the driver failed"},
            {"id": "verify.expect", "act": ["*"], "when": "ok and expect != 'any'",
             "assert": "changed == (expect == 'changed')",
             "why": "a step's outcome is declared on the step: expect = \"changed\" (the default for an action) or \"unchanged\""}
        ]
    })
}

fn rules_for(f: &Feature) -> Value {
    let mut r = f.rules.clone();
    let b = builtin_rules();
    for kind in ["invariant", "contract"] {
        let mut all = b[kind].as_array().cloned().unwrap_or_default();
        all.extend(r[kind].as_array().cloned().unwrap_or_default());
        r[kind] = Value::Array(all);
    }
    r
}

fn make_driver(t: &Target) -> Result<Box<dyn Driver>, String> {
    match t.driver().as_str() {
        "adb" => Ok(Box::new(Adb::new(t)?)),
        other => Err(format!("driver '{other}' is not built")),
    }
}

/// A read-only GET of a `[[probe]]`; the token goes on stdin, never on a command line.
fn probe(t: &Target, id: &str) -> Result<Value, String> {
    let p = t.probes().into_iter().find(|p| p["id"] == id).ok_or_else(|| format!("no probe '{id}'"))?;
    let url = expand(p["url"].as_str().unwrap_or(""));
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(format!("probe {id}: url must be http(s), got '{url}'"));
    }
    let tok = &p["token"];
    let token = tok["env"]
        .as_str()
        .and_then(|e| std::env::var(e).ok())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| tok["file"].as_str().and_then(|f| std::fs::read_to_string(evidence::tilde(f)).ok()))
        .map(|s| s.trim().to_string());
    let mut child = Command::new("curl")
        .args(["-sS", "-m", "15", "-H", "@-", "-H", "Accept: application/json", "-w", "\n%{http_code}", &url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("probe {id}: can't run curl: {e}"))?;
    {
        let mut stdin = child.stdin.take().unwrap();
        if let Some(tok) = token {
            let _ = writeln!(stdin, "Authorization: Bearer {tok}");
        }
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", text.as_str()));
    let code: u16 = code.trim().parse().unwrap_or(0);
    if !(200..300).contains(&code) {
        return Err(format!("probe {id}: HTTP {code} {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let v = serde_json::from_str::<Value>(body).map_err(|e| format!("probe {id}: not JSON ({e})"))?;
    Ok(json!({"status": code, "body": v}))
}

pub fn doctor_only(_root: &Path, t: &Target) -> i32 {
    let mut d = match make_driver(t) {
        Ok(d) => d,
        Err(e) => {
            println!("[error] {e}");
            return 2;
        }
    };
    let ok = doctor(t, d.as_mut(), true);
    if ok {
        println!("doctor: {} is worth driving", t.name);
        0
    } else {
        1
    }
}

fn doctor(t: &Target, d: &mut dyn Driver, print: bool) -> bool {
    let mut ok = true;
    for c in d.doctor(&t.strs("doctor", "checks")) {
        ok &= c.ok;
        if print || !c.ok {
            println!("  {} {:<10} {}", if c.ok { "ok  " } else { "FAIL" }, c.name, c.detail);
        }
    }
    if !ok {
        return false;
    }
    for p in t.strs("doctor", "probe") {
        match probe(t, &p) {
            Ok(_) => {
                if print {
                    println!("  ok   probe:{p}  answers");
                }
            }
            Err(e) => {
                println!("  FAIL probe:{p}  {e}");
                ok = false;
            }
        }
    }
    ok
}

struct Stream {
    file: std::fs::File,
    checker: Checker,
    t0: Instant,
    prefix: String,
}

impl Stream {
    fn emit(&mut self, mut obj: IndexMap<String, Value>) {
        obj.insert("t".into(), json!((self.t0.elapsed().as_secs_f64() * 10.0).round() / 10.0));
        let line = format!("{} {}", self.prefix, serde_json::to_string(&obj).unwrap());
        let _ = writeln!(self.file, "{line}");
        self.checker.feed_line(&line);
    }
}

fn label_file(n: usize, s: &str) -> String {
    let clean: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).take(60).collect();
    format!("{n:02}-{clean}")
}

fn snapshot(rec: &IndexMap<String, Value>) -> Value {
    json!({"focus": rec["focus"], "ids": rec["ids"], "els": rec["els"], "texts": rec["texts"]})
}

/// Run each feature; with `prove`, also check its evidence after cleanup and record the proof.
pub fn features(root: &Path, t: &Target, picked: &[&Feature], prove: bool) -> i32 {
    let base = match evidence::base(root, t) {
        Ok(b) => b,
        Err(e) => {
            println!("[error] {e}");
            return 2;
        }
    };
    let mut d = match make_driver(t) {
        Ok(d) => d,
        Err(e) => {
            println!("[error] {e}");
            return 2;
        }
    };
    println!("doctor ({})", t.name);
    if !doctor(t, d.as_mut(), true) {
        println!("\nseamcheck verify: the doctor says this instance is not worth driving; nothing was run");
        return 1;
    }
    let mut failed = 0;
    for f in picked {
        let (stamp, iso) = evidence::now();
        let dir = base.join(format!("{stamp}-{}", f.id));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            println!("[error] {}: {e}", dir.display());
            return 2;
        }
        println!("\n{} — {}\n  evidence: {}", f.id, f.title, dir.display());
        let vs = one(t, f, d.as_mut(), &dir);
        let errs = vs.iter().filter(|v| v.severity == "error").count();
        let mut ok = errs == 0;
        if prove && !ok {
            println!("  not proven: the run has errors");
        } else if prove {
            match proof_check(f, &dir, &vs) {
                Ok(()) => {
                    let app = std::fs::read_to_string(dir.join("meta.json"))
                        .ok()
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                        .map(|m| m["driven"].clone())
                        .unwrap_or(Value::Null);
                    match check::write_proof(t, f, &feature_hash(t, f), &iso, &app) {
                        Ok(p) => println!("  proven: evidence intact after cleanup; recorded in {}", p.display()),
                        Err(e) => {
                            println!("  [error] proof: {e}");
                            ok = false;
                        }
                    }
                }
                Err(e) => {
                    println!("  [error] proof: {e}");
                    ok = false;
                }
            }
        }
        if !ok {
            failed += 1;
        }
    }
    println!("\nseamcheck verify: {} of {} feature(s) clean", picked.len() - failed, picked.len());
    (failed > 0) as i32
}

/// The evidence survived cleanup and judges the same from disk.
fn proof_check(f: &Feature, dir: &Path, vs: &[Violation]) -> Result<(), String> {
    let stream = std::fs::read_to_string(dir.join("stream.jsonl")).map_err(|e| format!("stream.jsonl is gone after cleanup: {e}"))?;
    let mut again = Checker::new(&rules_for(f), false);
    for l in stream.lines() {
        again.feed_line(l);
    }
    let log = std::fs::read_to_string(dir.join("logcat.txt")).unwrap_or_default();
    for l in log.lines() {
        again.feed_line(l);
    }
    let re: Vec<_> = again.finish().into_iter().filter(|v| v.rule != "no-end").collect();
    let mine: Vec<&Violation> = vs.iter().filter(|v| v.rule != "no-end").collect();
    if re.len() != mine.len() {
        return Err(format!("the stream on disk judges differently ({} findings, the run had {})", re.len(), mine.len()));
    }
    let trees = std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "xml")).count();
    if trees < f.steps.len() {
        return Err(format!("{trees} trees on disk for {} steps", f.steps.len()));
    }
    for name in ["verdict.json", "meta.json"] {
        if !dir.join(name).exists() {
            return Err(format!("{name} is missing"));
        }
    }
    Ok(())
}

fn one(t: &Target, f: &Feature, d: &mut dyn Driver, dir: &Path) -> Vec<Violation> {
    let rules = rules_for(f);
    let prefix = t.raw.get("runtime").and_then(|r| r.get("prefix")).and_then(Value::as_str).unwrap_or("SEAMCHECK").to_string();
    let file = match std::fs::File::create(dir.join("stream.jsonl")) {
        Ok(x) => x,
        Err(e) => {
            println!("  [error] {e}");
            return vec![];
        }
    };
    let mut s = Stream { file, checker: Checker::new(&rules, false), t0: Instant::now(), prefix };
    let pkg = d.package().to_string();
    let full = f.capture == "full";
    let keep: &[String] = if full { &[] } else { &f.keep };
    let mut steps_note: Vec<String> = Vec::new();

    let save = |d: &mut dyn Driver, screen: &Screen, n: usize, label: &str| {
        let name = label_file(n, label);
        let xml = if full { screen.raw.clone() } else { screen.filtered_xml(&pkg, keep) };
        let _ = std::fs::write(dir.join(format!("{name}.xml")), xml);
        if full {
            if let Err(e) = d.screenshot(&dir.join(format!("{name}.png"))) {
                println!("  [warn] screenshot: {e}");
            }
        }
    };
    let state = |screen: &Screen, n: usize, label: &str, probes: &IndexMap<String, Value>| -> IndexMap<String, Value> {
        let mut m: IndexMap<String, Value> = IndexMap::new();
        m.insert("k".into(), json!("state"));
        m.insert("step".into(), json!(n));
        m.insert("label".into(), json!(label));
        m.insert("front".into(), json!(screen.focus.starts_with(&format!("{pkg}/"))));
        m.extend(screen.record(&pkg, keep));
        m.insert("probes".into(), Value::Object(probes.iter().map(|(k, v)| (k.clone(), v.clone())).collect()));
        m
    };

    let driven = d.describe();
    let started = match d.launch() {
        Ok(st) => st,
        Err(e) => {
            println!("  [error] launch: {e}");
            s.emit(IndexMap::from([("k".into(), json!("act")), ("act".into(), json!("launch")), ("ok".into(), json!(false)), ("expect".into(), json!("any")), ("changed".into(), json!(false)), ("error".into(), json!(e))]));
            // a launch that failed may still have started it
            let _ = d.cleanup(true);
            return finish(t, f, d, s, dir, false, &driven, &[]);
        }
    };
    let mut probes: IndexMap<String, Value> = IndexMap::new();
    let mut screen = match d.observe() {
        Ok(x) => x,
        Err(e) => {
            println!("  [error] observe: {e}");
            let _ = d.cleanup(started);
            return finish(t, f, d, s, dir, started, &driven, &[]);
        }
    };
    save(d, &screen, 0, "launch");
    let st = state(&screen, 0, "launch", &probes);
    s.emit(st);

    for (i, step) in f.steps.iter().enumerate() {
        let n = i + 1;
        let label = step.label();
        let before = screen.record(&pkg, keep);
        let sig = screen.signature(&pkg);
        let t_step = Instant::now();
        let result: Result<Screen, String> = match step.action.as_str() {
            "probe" => match probe(t, &step.arg) {
                Ok(v) => {
                    probes.insert(step.arg.clone(), v);
                    d.observe()
                }
                Err(e) => Err(e),
            },
            "wait" | "gone" => {
                let deadline = Instant::now() + Duration::from_secs_f64(step.timeout);
                loop {
                    let sc = match d.observe() {
                        Ok(sc) => sc,
                        Err(e) => break Err(e),
                    };
                    if sc.find(&step.arg, &pkg).is_some() == (step.action == "wait") {
                        break Ok(sc);
                    }
                    if Instant::now() > deadline {
                        break Err(format!(
                            "'{}' {} within {}s",
                            step.arg,
                            if step.action == "wait" { "did not appear" } else { "did not go" },
                            step.timeout
                        ));
                    }
                    if step.scroll.is_empty() {
                        std::thread::sleep(Duration::from_millis(400));
                        continue;
                    }
                    let dir = if step.dir.is_empty() { "up".to_string() } else { step.dir.clone() };
                    let swipe = Step { action: "swipe".into(), arg: step.scroll.clone(), dir, ..step.clone() };
                    if let Err(e) = d.act(&swipe, &sc) {
                        break Err(e);
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
            _ => steady(d, step, &pkg, screen.clone()).and_then(|now| d.act(step, &now)).and_then(|_| settle(d, step, &pkg, &sig)),
        };
        let (ok, err) = match result {
            Ok(sc) => {
                screen = sc;
                (true, String::new())
            }
            Err(e) => (false, e),
        };
        let after = screen.record(&pkg, keep);
        let changed = ok && screen.signature(&pkg) != sig;
        let mut act: IndexMap<String, Value> = IndexMap::new();
        act.insert("k".into(), json!("act"));
        act.insert("act".into(), json!(label));
        act.insert("step".into(), json!(n));
        act.insert("id".into(), json!(step.element().unwrap_or("")));
        act.insert("ok".into(), json!(ok));
        act.insert("expect".into(), json!(step.expect));
        act.insert("changed".into(), json!(changed));
        act.insert("secs".into(), json!((t_step.elapsed().as_secs_f64() * 10.0).round() / 10.0));
        if !err.is_empty() {
            act.insert("error".into(), json!(err));
        }
        act.insert("before".into(), snapshot(&before));
        act.insert("after".into(), snapshot(&after));
        s.emit(act);
        save(d, &screen, n, &label);
        s.emit(state(&screen, n, &label, &probes));
        let mark = if !ok { "FAIL" } else if step.expect != "any" && changed != (step.expect == "changed") { "UNEXPECTED" } else { "ok" };
        println!("  {n:>2} {mark:<4} {label}{}", if err.is_empty() { String::new() } else { format!("  ({err})") });
        steps_note.push(label);
        if !ok {
            break; // the rest of the journey depends on this step
        }
    }
    let cleaned = d.cleanup(started);
    match &cleaned {
        Ok(m) => println!("  cleanup: {m}"),
        Err(e) => println!("  [error] cleanup: {e}"),
    }
    let ended = cleaned.is_ok();
    finish(t, f, d, s, dir, ended, &driven, &steps_note)
}

/// The screen once the step's control holds still: a result list arriving can move a tab between the dump
/// and the touch, and the touch would land on its neighbour.
fn steady(d: &mut dyn Driver, step: &Step, pkg: &str, mut screen: Screen) -> Result<Screen, String> {
    let Some(id) = step.element() else { return Ok(screen) };
    for _ in 0..6 {
        let before = screen.find(id, pkg).map(|n| n.bounds);
        std::thread::sleep(Duration::from_millis(350));
        let now = d.observe()?;
        let after = now.find(id, pkg).map(|n| n.bounds);
        screen = now;
        if before == after {
            break;
        }
    }
    Ok(screen)
}

fn settle(d: &mut dyn Driver, step: &Step, pkg: &str, sig: &str) -> Result<Screen, String> {
    if step.expect == "unchanged" {
        std::thread::sleep(Duration::from_millis(1500));
        return d.observe();
    }
    let deadline = Instant::now() + Duration::from_secs_f64(step.timeout);
    std::thread::sleep(Duration::from_millis(400));
    loop {
        let sc = d.observe()?;
        if sc.signature(pkg) != sig || Instant::now() > deadline {
            if sc.signature(pkg) != sig {
                // let the change finish (a sheet sliding in, a list arriving)
                std::thread::sleep(Duration::from_millis(600));
                return d.observe();
            }
            return Ok(sc);
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[allow(clippy::too_many_arguments)]
fn finish(t: &Target, f: &Feature, d: &mut dyn Driver, mut s: Stream, dir: &Path, ended: bool, driven: &Value, steps: &[String]) -> Vec<Violation> {
    let fail_on: Vec<Regex> = t
        .raw
        .get("runtime")
        .and_then(|r| r.get("fail_on"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str().and_then(|p| Regex::new(p).ok()))
        .collect();
    match d.log() {
        Ok(log) => {
            let kept: Vec<&str> = if f.capture == "full" { log.lines().collect() } else { log.lines().filter(|l| fail_on.iter().any(|r| r.is_match(l))).collect() };
            let _ = std::fs::write(dir.join("logcat.txt"), kept.join("\n") + "\n");
            for l in &kept {
                s.checker.feed_line(l);
            }
        }
        Err(e) => println!("  [warn] log: {e}"),
    }
    if ended {
        s.emit(IndexMap::from([("k".into(), json!("end"))]));
    }
    let vs = s.checker.finish();
    for v in &vs {
        let why = rules_why(&rules_for(f), &v.rule);
        println!("  {}{}", format_violation(v).replace('\n', "\n  "), if why.is_empty() { String::new() } else { format!("\n      why: {why}") });
    }
    println!("  {}", s.checker.summary());
    let verdict: Vec<Value> = vs
        .iter()
        .map(|v| json!({"rule": v.rule, "kind": v.kind, "severity": v.severity, "t": v.t, "subject": v.subject, "message": v.message, "count": v.count}))
        .collect();
    let _ = std::fs::write(dir.join("verdict.json"), serde_json::to_string_pretty(&json!({"findings": verdict, "summary": s.checker.summary()})).unwrap());
    let meta = json!({
        "target": t.name, "feature": f.id, "title": f.title, "capture": f.capture,
        "hash": feature_hash(t, f), "driven": driven, "steps": steps,
        "rules": PathBuf::from(&f.file).display().to_string(),
    });
    let _ = std::fs::write(dir.join("meta.json"), serde_json::to_string_pretty(&meta).unwrap());
    vs
}

fn rules_why(rules: &Value, id: &str) -> String {
    for kind in ["invariant", "contract"] {
        for r in rules[kind].as_array().into_iter().flatten() {
            if r["id"] == id {
                return r["why"].as_str().unwrap_or("").to_string();
            }
        }
    }
    String::new()
}
