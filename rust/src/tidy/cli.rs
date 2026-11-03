//! Output for the gate and the tidy commands (tidy_cli.py): compact by
//! default (a screenful, not a context window), --json for tools, --ids for
//! piping into `seamcheck remove`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::Instant;

use indexmap::IndexMap;
use serde_json::{json, Value};

use super::exactness::{pyfloat, Book, FIX, WHY};
use super::{audit, dead, dups, remove, split, Corpus, Tidy};
use crate::config::{id, s};
use crate::gate::Report;
use crate::output::{dumps, github, sarif};
use crate::project::Project;

/// What the CLI hands these commands.
pub struct Opts<'a> {
    pub cmd: &'a str,
    pub args: &'a [String],
    pub config: &'a [String],
    pub format: &'a str,
    pub since: Option<&'a str>,
    pub staged: bool,
    pub limit: usize,
    pub ids: bool,
    pub apply: bool,
}

const FIX_FORBIDDEN: &str = "retire the old path instead of naming a second one";
const FIX_MOVED: &str = "point it at where the code went (or a glob), or `seamcheck: allow moved-ref`";

fn fix(k: &str) -> &'static str {
    match k {
        "forbidden" => FIX_FORBIDDEN,
        "unused" => "delete it, or call it from product code",
        "test_only" => "delete it with its tests, or wire it up",
        "dups" => "keep one copy and call it",
        "tests" => "make the test call product code (or delete it)",
        _ => FIX_MOVED,
    }
}

fn whys(p: &Project) -> HashMap<String, String> {
    p.config.rules().map(|r| (id(r).to_string(), s(r, "why").unwrap_or("").to_string())).collect()
}

/// One flat list an agent (or a CI annotator) can act on (tidy_cli.gate_findings).
pub fn gate_findings(p: &Project, rep: &Report, r: &str) -> Vec<Value> {
    let w = whys(p);
    let mut flat: Vec<Value> = rep
        .rules
        .iter()
        .map(|f| {
            json!({"kind": "rule", "rule": f.rule, "severity": f.severity, "file": f.file, "line": f.line,
                   "message": f.message, "why": w.get(&f.rule).cloned().unwrap_or_default(), "fix": ""})
        })
        .collect();
    let row = |kind: &str, file: &str, line: usize, message: &str| {
        json!({"kind": kind, "rule": kind, "severity": "error", "file": file, "line": line, "message": message,
               "why": "", "fix": fix(kind)})
    };
    flat.extend(rep.forbidden.iter().map(|x| row("forbidden", &x.file, x.line, &x.word)));
    flat.extend(rep.unused.iter().map(|x| row("unused", &x.file, x.line, &x.id)));
    flat.extend(rep.test_only.iter().map(|x| row("test_only", &x.file, x.line, &x.id)));
    flat.extend(rep.tests.iter().map(|x| row("tests", &x.file, x.line, &x.id)));
    for x in &rep.moved_refs {
        flat.push(json!({"kind": "moved_refs", "rule": "moved-ref", "severity": "error", "file": x.file, "line": x.line,
                         "message": format!("names {}, which this change split up", x.moved_from),
                         "why": "a check pinned to one file checks less after a split", "fix": FIX_MOVED}));
    }
    for d in &rep.dups {
        flat.push(json!({"kind": "dups", "rule": "dups", "severity": "error", "file": d.a, "line": 0,
                         "message": format!("copy of {} ({})", d.b, pyfloat(d.similarity)), "why": "", "fix": fix("dups")}));
    }
    for d in &rep.exactness {
        flat.push(json!({"kind": "exactness", "rule": "exactness", "severity": "warn", "file": d.file, "line": 0,
                         "message": d.message(r), "why": WHY, "fix": FIX}));
    }
    flat
}

pub fn gate(o: &Opts, root: &Path, t0: Instant) -> i32 {
    let mut p = Project::load(root, o.config);
    let r = o.since.unwrap_or("HEAD");
    let rep = crate::gate::gate_with(&mut p, r, o.staged, o.config, &Book::default());
    let bad = rep.failed();
    if o.format != "text" {
        let flatv = gate_findings(&p, &rep, r);
        match o.format {
            "json" => println!(
                "{}",
                dumps(&json!({"blocked": bad, "changed_files": rep.changed_files, "findings": flatv,
                              "ms": t0.elapsed().as_millis() as u64}))
            ),
            "sarif" => println!("{}", sarif(&flatv, &[])),
            _ => println!("{}", github(&flatv)),
        }
        return bad as i32;
    }
    let w = whys(&p);
    let mut shown = std::collections::HashSet::new();
    for f in &rep.rules {
        println!("  rule {}: {}:{}  {}", f.rule, f.file, f.line, f.message);
        let why = w.get(&f.rule).map(|x| x.as_str()).unwrap_or("");
        if !why.is_empty() && shown.insert(f.rule.clone()) {
            println!("      why: {why}");
        }
    }
    for x in &rep.forbidden {
        println!("  new code names '{}': {}:{}  (retire the old path instead)", x.word, x.file, x.line);
    }
    for x in &rep.unused {
        println!("  unused (nothing calls or names it): {}  {}:{}", x.id, x.file, x.line);
    }
    for x in &rep.test_only {
        println!("  only tests call it: {}  {}:{}", x.id, x.file, x.line);
    }
    for d in &rep.dups {
        println!("  copy ({}): {}  ~  {}", pyfloat(d.similarity), d.a, d.b);
    }
    for x in &rep.tests {
        let why = if x.reads_source.is_empty() {
            "reaches no product code".to_string()
        } else {
            format!("reads {}", x.reads_source.join(", "))
        };
        let short = x.id.rsplit_once(".test ").map(|s| s.1).unwrap_or(&x.id);
        println!("  test {why}: {short}  {}:{}", x.file, x.line);
    }
    for x in &rep.moved_refs {
        println!(
            "  names {}, which this change split up: {}:{}  (point it at the new files or a glob)",
            x.moved_from, x.file, x.line
        );
    }
    for d in &rep.exactness {
        println!("  warning (not blocking): {}\n      why: {WHY}; {FIX}", d.message(r));
    }
    println!(
        "seamcheck gate: {} changed files; {}  ({} ms)",
        rep.changed_files.len(),
        if bad { "BLOCKED" } else { "clear" },
        t0.elapsed().as_millis()
    );
    bad as i32
}

fn basename(f: &str) -> &str {
    f.rsplit('/').next().unwrap_or(f)
}

/// Counter.most_common: by count, ties in first-seen order.
fn most_common<'a>(xs: impl Iterator<Item = &'a str>) -> Vec<(&'a str, usize)> {
    let mut c: IndexMap<&str, usize> = IndexMap::new();
    for x in xs {
        *c.entry(x).or_insert(0) += 1;
    }
    let mut v: Vec<(&str, usize)> = c.into_iter().collect();
    v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    v
}

fn tests_summary(rows: &[audit::TestRow], limit: usize) -> Vec<String> {
    let mut by: BTreeMap<&str, Vec<&audit::TestRow>> = BTreeMap::new();
    for r in rows {
        by.entry(&r.suite).or_default().push(r);
    }
    let mut out = Vec::new();
    for (s, rs) in &by {
        let zero = rs.iter().filter(|r| r.reach == 0).count();
        let grep = rs.iter().filter(|r| !r.reads_source.is_empty()).count();
        out.push(format!("  {s}: {} tests, {zero} reach no product code, {grep} read product source", rs.len()));
    }
    let files = most_common(rows.iter().filter(|r| r.reach == 0).map(|r| r.file.as_str()));
    if !files.is_empty() {
        let n = (limit / 2).max(1);
        out.push(format!(
            "  files with the most tests reaching nothing: {}",
            files.iter().take(n).map(|(f, n)| format!("{} {n}", basename(f))).collect::<Vec<_>>().join(", ")
        ));
    }
    out
}

fn rows_json(rows: &[dead::Row]) -> Value {
    Value::Array(rows.iter().map(|r| r.json()).collect())
}

/// tidy, tests, test-only, unused, dups, split, hotspots, remove, refs.
pub fn main(o: &Opts, root: &Path, t0: Instant) -> i32 {
    let p = Project::load(root, o.config);
    let g = p.graph();
    let t = Tidy::new(&p, &g);
    let lim = o.limit;
    let json_out = o.format == "json";
    let only: Option<BTreeSet<String>> = o.since.map(|r| super::changed_files(&p, r));
    let keep = |rel: &str| only.as_ref().map_or(true, |s| s.contains(rel));
    match o.cmd {
        "refs" => {
            if o.args.is_empty() {
                println!("refs needs a name");
                return 2;
            }
            let corpus = Corpus::new(&p);
            for name in o.args {
                let r = super::refs(&t, name, &corpus);
                if json_out {
                    println!("{}", dumps(&r));
                    continue;
                }
                println!("{name}:");
                for k in ["defined", "callers", "mentions"] {
                    let vals: Vec<&str> = r[k].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
                    println!(
                        "  {k} ({}): {}{}",
                        vals.len(),
                        if vals.is_empty() { "-".to_string() } else { vals.iter().take(lim).copied().collect::<Vec<_>>().join("; ") },
                        if vals.len() > lim { " ..." } else { "" }
                    );
                }
            }
            0
        }
        "remove" => {
            if o.args.is_empty() {
                println!("remove needs function ids (as `seamcheck find` / `--ids` print them)");
                return 2;
            }
            match remove::remove(&t, o.args, !o.apply, true) {
                Err(e) => {
                    println!("{e}");
                    1
                }
                Ok(done) => {
                    for line in done {
                        println!("{}{line}", if o.apply { "removed " } else { "would remove " });
                    }
                    if !o.apply {
                        println!("(dry run: --apply writes the files)");
                    }
                    0
                }
            }
        }
        "split" => {
            if o.args.is_empty() {
                println!("split needs a file");
                return 2;
            }
            for rel in o.args {
                let r = split::split(&t, rel);
                if json_out {
                    println!("{}", dumps(&r));
                    continue;
                }
                println!(
                    "{rel}: {} functions, {} lines{}",
                    r["functions"],
                    r["lines"],
                    if r["knotted"].as_bool() == Some(true) {
                        "  (knotted: one call cluster holds most of it; split by feature)"
                    } else {
                        ""
                    }
                );
                let feats: Vec<String> = r["features"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .take(lim)
                    .map(|x| format!("{} {}", x["name"].as_str().unwrap_or(""), x["lines"]))
                    .collect();
                println!("  features: {}", feats.join(", "));
                let helpers: Vec<&str> = r["shared_helpers"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
                if !helpers.is_empty() {
                    println!("  shared helpers (move out first): {}", helpers.iter().take(lim).copied().collect::<Vec<_>>().join(", "));
                }
                for c in r["clusters"].as_array().unwrap().iter().take(lim) {
                    let ex: Vec<&str> = c["examples"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
                    println!("  cluster {} fns / {} lines: {}", c["functions"], c["lines"], ex.join(", "));
                }
            }
            0
        }
        "hotspots" => {
            let rows = split::hotspots(&t, lim);
            if json_out {
                println!("{}", dumps(&Value::Array(rows.iter().map(|r| r.json()).collect())));
            } else {
                for r in &rows {
                    println!(
                        "  {:>6} lines {:>4} fns  called from {:>3} files  {}",
                        r.lines, r.functions, r.caller_files, r.file
                    );
                }
            }
            0
        }
        "dups" => {
            let groups: Vec<dups::Group> =
                dups::dups(&t, 12, 0.45, None).into_iter().filter(|gr| keep(&gr.files[0]) || keep(&gr.files[1])).collect();
            if json_out {
                println!("{}", dumps(&Value::Array(groups.iter().map(|gr| gr.json()).collect())));
            } else {
                for gr in groups.iter().take(lim) {
                    let names: BTreeSet<&str> = gr.pairs.iter().map(|x| x.a.rsplit('.').next().unwrap_or(&x.a)).collect();
                    let names: Vec<&str> = names.into_iter().take(5).collect();
                    println!(
                        "  {:>5} lines in {:>3} fns  {}  ~  {}  [{}]",
                        gr.lines,
                        gr.pairs.len(),
                        gr.files[0],
                        gr.files[1],
                        names.join(", ")
                    );
                }
                println!(
                    "seamcheck dups: {} file pairs, {} duplicated lines",
                    groups.len(),
                    groups.iter().map(|gr| gr.lines).sum::<usize>()
                );
            }
            0
        }
        "test-only" | "unused" => {
            let corpus = Corpus::new(&p);
            let rows: Vec<dead::Row> = if o.cmd == "test-only" { dead::test_only(&t, &corpus) } else { dead::unused(&t, &corpus) }
                .into_iter()
                .filter(|r| keep(&r.file))
                .collect();
            if o.ids {
                println!("{}", rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>().join("\n"));
            } else if json_out {
                println!("{}", dumps(&rows_json(&rows)));
            } else {
                for r in rows.iter().take(lim) {
                    let extra = match &r.tests {
                        Some(ts) => {
                            let mods: BTreeSet<&str> = ts
                                .iter()
                                .map(|t| {
                                    let head = t.split(".test ").next().unwrap_or(t);
                                    head.rsplit_once('.').map(|x| x.0).unwrap_or(head)
                                })
                                .collect();
                            format!("  <- {}", mods.into_iter().take(3).collect::<Vec<_>>().join(", "))
                        }
                        None => String::new(),
                    };
                    println!("  {:>4} {}  {}:{}{extra}", r.lines, r.id, r.file, r.line);
                }
                println!(
                    "seamcheck {}: {} functions, {} lines",
                    o.cmd,
                    rows.len(),
                    rows.iter().map(|r| r.lines).sum::<usize>()
                );
            }
            0
        }
        "tests" => {
            let rows: Vec<audit::TestRow> = audit::test_audit(&t, None).into_iter().filter(|r| keep(&r.file)).collect();
            if o.ids {
                let ids: Vec<&str> =
                    rows.iter().filter(|r| r.reach == 0 || !r.reads_source.is_empty()).map(|r| r.id.as_str()).collect();
                println!("{}", ids.join("\n"));
            } else if json_out {
                println!("{}", dumps(&Value::Array(rows.iter().map(|r| r.json()).collect())));
            } else {
                println!("{}", tests_summary(&rows, lim).join("\n"));
                for r in rows.iter().filter(|r| !r.reads_source.is_empty()).take(lim) {
                    let short = r.id.rsplit('/').next().unwrap_or(&r.id);
                    let reads: Vec<&str> = r.reads_source.iter().take(2).map(|s| s.as_str()).collect();
                    println!("  reads {}: {short}  {}:{}", reads.join(", "), r.file, r.line);
                }
            }
            0
        }
        _ => tidy_report(&t, o, only.as_ref(), t0),
    }
}

/// `seamcheck tidy`: everything, briefly.
fn tidy_report(t: &Tidy, o: &Opts, only: Option<&BTreeSet<String>>, t0: Instant) -> i32 {
    let keep = |rel: &str| only.map_or(true, |s| s.contains(rel));
    let lim = o.limit;
    let corpus = Corpus::new(t.p);
    let hot: Vec<split::Hot> = split::hotspots(t, 400).into_iter().filter(|r| keep(&r.file)).take(8).collect();
    let d: Vec<dups::Group> =
        dups::dups(t, 12, 0.45, None).into_iter().filter(|gr| keep(&gr.files[0]) || keep(&gr.files[1])).collect();
    let test_only: Vec<dead::Row> = dead::test_only(t, &corpus).into_iter().filter(|r| keep(&r.file)).collect();
    let unused: Vec<dead::Row> = dead::unused(t, &corpus).into_iter().filter(|r| keep(&r.file)).collect();
    let tests: Vec<audit::TestRow> = audit::test_audit(t, None).into_iter().filter(|r| keep(&r.file)).collect();
    if let (Some(s), Some(r)) = (only, o.since) {
        println!("(only files changed since {r}: {})", s.len());
    }
    if o.format == "json" {
        println!(
            "{}",
            dumps(&json!({
                "hotspots": hot.iter().map(|r| r.json()).collect::<Vec<_>>(),
                "dups": d.iter().map(|gr| gr.json()).collect::<Vec<_>>(),
                "test_only": rows_json(&test_only),
                "unused": rows_json(&unused),
                "tests": tests.iter().map(|r| r.json()).collect::<Vec<_>>(),
            }))
        );
        return 0;
    }
    println!("biggest files:");
    for r in &hot {
        println!("  {:>6} lines {:>4} fns  {}", r.lines, r.functions, r.file);
    }
    println!("duplication: {} file pairs, {} lines", d.len(), d.iter().map(|gr| gr.lines).sum::<usize>());
    for gr in d.iter().take(lim.min(6)) {
        println!("  {:>5} lines  {}  ~  {}", gr.lines, gr.files[0], gr.files[1]);
    }
    for (rows, label) in [(&test_only, "only tests call"), (&unused, "nothing calls or names")] {
        println!("{label}: {} functions, {} lines", rows.len(), rows.iter().map(|r| r.lines).sum::<usize>());
        for r in rows.iter().take(lim.min(6)) {
            println!("  {:>4} {}", r.lines, r.id);
        }
    }
    println!("tests:");
    println!("{}", tests_summary(&tests, lim).join("\n"));
    println!(
        "(seamcheck tidy: {} ms; each list in full: seamcheck test-only | unused | dups | tests)",
        t0.elapsed().as_millis()
    );
    0
}
