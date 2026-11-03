//! The query commands' printing (__main__.py's `callers`, `callees`, `path`,
//! `find`, `context`, `search`, `impact`, `dead`, `stats` and `map`), line
//! for line. Each command writes into a `Ran`, so `seamcheck` prints it and
//! the parity harness runs hundreds of queries over one loaded graph.

use std::collections::BTreeSet;
use std::fmt::Write;

use serde_json::{json, Value};

use super::Q;
use crate::output::{dumps, dumps_line};
use crate::pyre::repr;

/// What a query command takes from the command line.
#[derive(Clone, Debug)]
pub struct Opts {
    pub args: Vec<String>,
    pub json: bool,
    pub depth: i64,
    pub fuzzy: bool,
    pub wide: bool,
    pub limit: i64,
    pub code: bool,
    pub since: Option<String>,
    pub staged: bool,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts { args: vec![], json: false, depth: 2, fuzzy: false, wide: false, limit: 20, code: false, since: None, staged: false }
    }
}

/// A command's exit code and output.
#[derive(Default, Debug)]
pub struct Ran {
    pub code: i32,
    pub out: String,
    pub err: String,
}

impl Ran {
    /// argparse's `ap.error`: usage on stderr, exit 2.
    fn usage(mut self, msg: &str) -> Ran {
        let _ = writeln!(self.err, "usage: seamcheck [-h] [--version] ...\nseamcheck: error: {msg}");
        self.code = 2;
        self
    }
}

macro_rules! out {
    ($r:expr, $($t:tt)*) => { let _ = writeln!($r.out, $($t)*); };
}

/// Python's `l[:n]` for a list length `len` (a negative n counts from the end).
fn py_take(len: usize, n: i64) -> usize {
    if n >= 0 {
        (n as usize).min(len)
    } else {
        len.saturating_sub(n.unsigned_abs() as usize)
    }
}

fn depth(n: i64) -> usize {
    n.max(0) as usize
}

impl<'a> Q<'a> {
    /// `_fmt`: an id with its place and the bridges it sits on.
    pub fn fmt(&self, fid: &str) -> String {
        let w = self.where_(fid);
        let file = w["file"].as_str().unwrap_or("");
        let mut s = if file.is_empty() { fid.to_string() } else { format!("{fid}  ({file}:{})", w["line"]) };
        let via: Vec<&str> = w["via"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
        if !via.is_empty() {
            let _ = write!(s, "  [via {}]", via.join(", "));
        }
        s
    }

    /// Run one query command (`cmd` as typed) and return what it prints.
    pub fn command(&self, cmd: &str, a: &Opts) -> Ran {
        match cmd {
            "impact" => self.cmd_impact(a),
            "callers" | "callees" => self.cmd_walk(cmd, a),
            "path" => self.cmd_path(a),
            "context" => self.cmd_context(a),
            "search" => self.cmd_search(a),
            "find" => self.cmd_find(a),
            "dead" => self.cmd_dead(),
            "stats" => self.cmd_stats(a, None),
            other => Ran { code: 2, err: format!("seamcheck: {other} is not a query command\n"), ..Default::default() },
        }
    }

    fn cmd_impact(&self, a: &Opts) -> Ran {
        let mut r = Ran::default();
        if a.args.is_empty() && a.since.is_none() {
            return r.usage("impact needs a function spec, a file, or --since REF");
        }
        let mut start: BTreeSet<String> = BTreeSet::new();
        if let Some(since) = &a.since {
            start = self.changed_functions(since, a.staged);
            if start.is_empty() {
                out!(r, "no functions changed since {since}");
                return r;
            }
            if !a.json {
                out!(r, "since {since}: {} functions changed", start.len());
            }
        }
        start.extend(self.target_functions(&a.args));
        if start.is_empty() {
            out!(r, "nothing matches {}", a.args.join(" "));
            r.code = 1;
            return r;
        }
        let d = if a.depth == 2 { a.depth.max(4) } else { a.depth };
        let (by, own) = self.impact(&start, depth(d), a.fuzzy);
        if a.json {
            out!(r, "{}", dumps(&Q::impact_json(&by)));
            return r;
        }
        out!(r, "change to {} functions in {} reaches:", start.len(), own.iter().cloned().collect::<Vec<_>>().join(", "));
        let mut areas: Vec<(&String, &Vec<(usize, String)>)> = by.iter().collect();
        areas.sort_by_key(|(k, v)| (own.contains(*k), std::cmp::Reverse(v.len())));
        for (k, v) in areas {
            let files: BTreeSet<&str> = v.iter().filter_map(|(_, f)| self.g.func(f)).map(|f| f.file.as_str()).collect();
            let mine = own.contains(k);
            out!(r, "  {k}: {} functions in {} files{}", v.len(), files.len(), if mine { "" } else { "   <- another view: check it" });
            let n = if mine { a.limit.min(5) } else { a.limit };
            for (d, fid) in &v[..py_take(v.len(), n)] {
                out!(r, "    {}{}", "  ".repeat(d - 1), self.fmt(fid));
            }
        }
        r
    }

    fn cmd_walk(&self, cmd: &str, a: &Opts) -> Ran {
        let mut r = Ran::default();
        let Some(spec) = a.args.first() else { return r.usage(&format!("{cmd} needs a function")) };
        let rev = cmd == "callers";
        let wide = rev && a.wide;
        let starts = self.walk(spec, depth(a.depth), rev, a.fuzzy, wide);
        if starts.is_empty() {
            out!(r, "no function matches {}", repr(spec));
            r.code = 1;
            return r;
        }
        let word = if rev { "callers" } else { "callees" };
        for s in &starts {
            let (ex, guess) = (s["exact"].as_u64().unwrap_or(0), s["guess"].as_u64().unwrap_or(0));
            let id = s["id"].as_str().unwrap_or("");
            out!(r, "{}  [{ex} exact, {guess} name-guess {word}{}]", self.fmt(id), if a.fuzzy || wide { "" } else { ", walking exact" });
            for x in s["reach"].as_array().into_iter().flatten() {
                let d = x["depth"].as_u64().unwrap_or(1) as usize;
                out!(r, "  {}{} {}", "  ".repeat(d - 1), if rev { "<-" } else { "->" }, self.fmt(x["id"].as_str().unwrap_or("")));
            }
            if ex == 0 && !a.fuzzy && !wide {
                // (a miss must not read as "nothing calls it")
                let tail = if guess > 0 {
                    format!("; {guess} by name guess only (--fuzzy shows them)")
                } else {
                    "; none by name guess either".to_string()
                };
                out!(r, "  0 exact {word}{tail}");
            }
            if wide {
                for x in s["possible_sites"].as_array().into_iter().flatten() {
                    out!(
                        r,
                        "  ?? {} line {}  (a call by that name on a library-typed value)",
                        self.fmt(x["id"].as_str().unwrap_or("")),
                        x["line"]
                    );
                }
            } else if rev && s["possible"].as_u64().unwrap_or(0) > 0 {
                out!(r, "  ({} more calls by that name on library-typed values: --wide lists them)", s["possible"]);
            }
        }
        r
    }

    fn cmd_path(&self, a: &Opts) -> Ran {
        let mut r = Ran::default();
        if a.args.len() < 2 {
            return r.usage("path needs FROM and TO");
        }
        let (best, through) = self.shortest(&a.args[0], &a.args[1], a.fuzzy);
        if best.is_empty() {
            if through {
                out!(r, "no exact call chain; one exists through name guesses (--fuzzy shows it)");
            } else {
                out!(r, "no call chain found");
            }
            r.code = 1;
            return r;
        }
        for (i, fid) in best.iter().enumerate() {
            out!(r, "{}{}{}", "  ".repeat(i), if i > 0 { "-> " } else { "" }, self.fmt(fid));
        }
        r
    }

    fn cmd_context(&self, a: &Opts) -> Ran {
        let mut r = Ran::default();
        let spec = a.args.first().map(|s| s.as_str()).unwrap_or("*");
        let got = self.context(spec, 80, 4);
        if a.json {
            out!(r, "{}", dumps(&Value::Array(got.clone())));
            r.code = got.is_empty() as i32;
            return r;
        }
        if got.is_empty() {
            out!(r, "nothing matches {spec}");
            r.code = 1;
            return r;
        }
        let s = |v: &Value| v.as_str().unwrap_or("").to_string();
        for c in &got {
            let callers = c["callers"].as_array().cloned().unwrap_or_default();
            let ex = callers.iter().filter(|x| x["exact"] == json!(true)).count();
            out!(r, "== {}  {}:{}-{}", s(&c["id"]), s(&c["file"]), c["start"], c["end"]);
            out!(r, "{}", s(&c["source"]));
            out!(r, "-- callers: {ex} exact, {} name-guess", callers.len() - ex);
            for x in callers.iter().take(25) {
                let mark = if x["exact"] == json!(true) { "" } else { "? " };
                out!(r, "   {mark}{}  ({}:{})", s(&x["id"]), s(&x["file"]), x["line"]);
            }
            out!(r, "-- calls");
            for x in c["callees"].as_array().into_iter().flatten() {
                let targets: Vec<String> = x["targets"].as_array().into_iter().flatten().map(s).collect();
                let to = if targets.is_empty() { "(library / unresolved)".to_string() } else { targets.join(", ") };
                let mark = if x["exact"] == json!(true) || targets.is_empty() { "" } else { "? " };
                out!(r, "   {}: {} -> {mark}{to}", x["line"], s(&x["name"]));
            }
            let tests = c["tests"].as_array().cloned().unwrap_or_default();
            out!(r, "-- tests reaching it ({}): run these after changing it", tests.len());
            for t in tests.iter().take(25) {
                out!(r, "   {}  ({}:{})", s(&t["id"]), s(&t["file"]), t["line"]);
            }
        }
        r
    }

    fn cmd_search(&self, a: &Opts) -> Ran {
        let mut r = Ran::default();
        let Some(pat) = a.args.first() else { return r.usage("search needs a regex (then optional file globs)") };
        let limit = if a.limit == 20 { a.limit.max(200) } else { a.limit };
        let rows = match self.search(pat, &a.args[1..], a.code, limit.max(1) as usize) {
            Ok(rows) => rows,
            Err(e) => {
                let _ = writeln!(r.err, "seamcheck search: bad regex {}: {e}", repr(pat));
                r.code = 1;
                return r;
            }
        };
        if a.json {
            out!(r, "{}", dumps(&Value::Array(rows.clone())));
            r.code = rows.is_empty() as i32;
            return r;
        }
        for x in &rows {
            let s = |v: &Value| v.as_str().unwrap_or("").to_string();
            let wh = if x["func"].is_null() {
                String::new()
            } else {
                format!("  [{}: {} exact, {} name-guess callers]", s(&x["func"]), x["exact_callers"], x["guess_callers"])
            };
            out!(r, "{}:{}: {}{wh}", s(&x["file"]), x["line"], s(&x["text"]));
        }
        r.code = rows.is_empty() as i32;
        r
    }

    fn cmd_find(&self, a: &Opts) -> Ran {
        let mut r = Ran::default();
        for f in self.find(a.args.first().map(|s| s.as_str()).unwrap_or("*")) {
            out!(
                r,
                "{}  {}:{}-{}  calls={} callers: {} exact, {} name-guess",
                f["id"].as_str().unwrap_or(""),
                f["file"].as_str().unwrap_or(""),
                f["start"],
                f["end"],
                f["calls"],
                f["exact_callers"],
                f["guess_callers"]
            );
        }
        r
    }

    fn cmd_dead(&self) -> Ran {
        let mut r = Ran::default();
        let mut dead = self.g.dead();
        dead.sort_by(|a, b| (&a.file, a.start).cmp(&(&b.file, b.start)));
        for f in dead {
            out!(r, "{}:{}  {}", f.file, f.start, f.id);
        }
        r
    }

    /// `stats`; `total_ms`: the whole run so far (the CLI's clock), else the load's.
    pub fn cmd_stats(&self, a: &Opts, total_ms: Option<u128>) -> Ran {
        let mut r = Ran::default();
        let st = self.stats();
        let (edges, exact) = (st["edges"].as_u64().unwrap_or(0), st["exact_edges"].as_u64().unwrap_or(0));
        let langs = st["languages"]
            .as_object()
            .map(|m| m.iter().map(|(k, v)| format!("{}: {v}", repr(k))).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        out!(
            r,
            "root {}\nfiles {} (parsed {}: {{{langs}}})\nmodules {}  functions {}\ncall edges {edges} (exact {exact}, fuzzy {})\nload {} ms, total {} ms",
            st["root"].as_str().unwrap_or(""),
            st["files"],
            st["parsed"],
            st["modules"],
            st["functions"],
            edges - exact,
            st["load_ms"],
            total_ms.unwrap_or(self.g.load_ms)
        );
        out!(r, "call sites resolved (exact / name guess only / nothing), and of the calls that reach project");
        out!(r, "code (exact + guess) how many are certain; \"nothing\" is mostly the language's own library,");
        out!(r, "and \"named\" is the part of it a project function's name matches (a resolver that turns");
        out!(r, "guesses into nothing shows up there):");
        let res = st["resolution"].as_object().cloned().unwrap_or_default();
        if a.json {
            let short: serde_json::Map<String, Value> = res
                .iter()
                .map(|(ext, x)| (ext.clone(), json!({"exact": x["exact"], "guess": x["guess"], "nothing": x["nothing"], "named": x["named"]})))
                .collect();
            out!(r, "{}", dumps_line(&Value::Object(short)));
            return r;
        }
        for (ext, x) in &res {
            let n = x["calls"].as_u64().unwrap_or(0);
            if n == 0 {
                continue;
            }
            let pct = |k: &str| 100 * x[k].as_u64().unwrap_or(0) / n;
            let own = match x["project_exact"].as_u64() {
                Some(pe) => format!("project calls exact {pe:>3}%"),
                None => String::new(),
            };
            let label = if ext.is_empty() { "(none)" } else { ext.as_str() };
            out!(
                r,
                "  {label:<6} {n:>7}  exact {:>3}%  guess {:>3}%  nothing {:>3}% (named {:>2}%)   {own}",
                pct("exact"),
                pct("guess"),
                pct("nothing"),
                pct("named")
            );
        }
        r
    }
}
