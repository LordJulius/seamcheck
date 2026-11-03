//! `parity --tidy`: each tidy command's output from both implementations.
//! JSON compares as values; where Python's order comes from a set (dups'
//! pairs, ties between groups, split's cluster examples after the first),
//! both sides are put in one order first. Text compares line for line,
//! timing lines aside.

use std::path::Path;

use serde_json::Value;

pub struct Side<'a> {
    pub run: &'a dyn Fn(&[&str]) -> (String, u128),
}

fn json(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or(Value::Null)
}

/// Python's json.dumps(indent=1) prints one value per call; refs / split print one per name.
fn json_stream(s: &str) -> Vec<Value> {
    serde_json::Deserializer::from_str(s).into_iter::<Value>().filter_map(|v| v.ok()).collect()
}

fn norm_dups(v: &mut Value) {
    let Some(groups) = v.as_array_mut() else { return };
    for g in groups.iter_mut() {
        if let Some(p) = g.get_mut("pairs").and_then(|p| p.as_array_mut()) {
            p.sort_by_key(|x| (x["a"].as_str().unwrap_or("").to_string(), x["b"].as_str().unwrap_or("").to_string()));
        }
    }
    groups.sort_by_key(|g| {
        (std::cmp::Reverse(g["lines"].as_u64().unwrap_or(0)), g["files"].to_string())
    });
}

fn norm_split(v: &mut Value) {
    if let Some(cs) = v.get_mut("clusters").and_then(|c| c.as_array_mut()) {
        for c in cs {
            if let Some(ex) = c.get_mut("examples").and_then(|e| e.as_array_mut()) {
                ex.truncate(1); // (after the first, Python's walk follows a set's order)
            }
        }
    }
}

fn text_lines(s: &str) -> Vec<String> {
    s.lines().filter(|l| !l.contains(" ms)") && !l.contains(" ms;")).map(String::from).collect()
}

fn diff_values(label: &str, a: &Value, b: &Value) -> usize {
    if a == b {
        return 0;
    }
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            let mut n = 0;
            for i in 0..x.len().max(y.len()) {
                let (p, r) = (x.get(i).unwrap_or(&Value::Null), y.get(i).unwrap_or(&Value::Null));
                if p != r {
                    n += 1;
                    if n <= crate::max_show() {
                        println!("  {label}[{i}] python: {p}\n  {label}[{i}] rust:   {r}");
                    }
                }
            }
            if x.len() != y.len() {
                println!("  {label}: {} rows vs {}", x.len(), y.len());
            }
            n.max(1)
        }
        (Value::Object(x), Value::Object(y)) => {
            let mut n = 0;
            for k in x.keys().chain(y.keys().filter(|k| !x.contains_key(*k))) {
                n += diff_values(&format!("{label}.{k}"), x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null));
            }
            n
        }
        _ => {
            println!("  {label} python: {a}\n  {label} rust:   {b}");
            1
        }
    }
}

fn diff_text(label: &str, a: &str, b: &str, as_bag: bool) -> usize {
    let (mut x, mut y) = (text_lines(a), text_lines(b));
    if as_bag {
        x.sort();
        y.sort();
    }
    if x == y {
        return 0;
    }
    let mut n = 0;
    for i in 0..x.len().max(y.len()) {
        if x.get(i) != y.get(i) {
            n += 1;
            if n <= crate::max_show() {
                println!("  {label} line {i} python: {:?}\n  {label} line {i} rust:   {:?}", x.get(i), y.get(i));
            }
        }
    }
    n
}

/// Every tidy command on one repo; returns the number of differences.
pub fn tidy_parity(py: &Side, rs: &Side, repo: &Path, name: &str) -> usize {
    let _ = repo;
    let mut total = 0;
    let mut row = |what: &str, n: usize, pms: u128, rms: u128| {
        println!("{:<40} {:>9} {:>9} {:>9} {:>9}", format!("{name} {what}"), if n == 0 { "same" } else { "DIFF" }, n, pms, rms);
        total += n;
    };
    // JSON reports
    for (cmd, dups_like) in [("tests", false), ("test-only", false), ("unused", false), ("dups", true), ("tidy", false)] {
        let ((p, pms), (r, rms)) = ((py.run)(&[cmd, "--json"]), (rs.run)(&[cmd, "--json"]));
        let (mut pv, mut rv) = (json(&p), json(&r));
        if dups_like {
            norm_dups(&mut pv);
            norm_dups(&mut rv);
        }
        if cmd == "tidy" {
            norm_dups(&mut pv["dups"]);
            norm_dups(&mut rv["dups"]);
        }
        if pv.is_null() {
            println!("  {cmd}: python printed no JSON: {}", p.lines().next().unwrap_or(""));
        }
        row(&format!("{cmd} --json"), diff_values(cmd, &pv, &rv), pms, rms);
    }
    let ((p, pms), (r, rms)) = ((py.run)(&["hotspots", "--json", "--limit", "60"]), (rs.run)(&["hotspots", "--json", "--limit", "60"]));
    let hot = json(&r);
    row("hotspots --json", diff_values("hotspots", &json(&p), &hot), pms, rms);
    // split on the biggest files
    let files: Vec<String> =
        hot.as_array().map(|a| a.iter().take(6).filter_map(|h| h["file"].as_str().map(String::from)).collect()).unwrap_or_default();
    if !files.is_empty() {
        let mut args: Vec<&str> = vec!["split", "--json"];
        args.extend(files.iter().map(|s| s.as_str()));
        let ((p, pms), (r, rms)) = ((py.run)(&args), (rs.run)(&args));
        let (mut pv, mut rv) = (json_stream(&p), json_stream(&r));
        pv.iter_mut().for_each(norm_split);
        rv.iter_mut().for_each(norm_split);
        row("split --json", diff_values("split", &Value::Array(pv), &Value::Array(rv)), pms, rms);
    }
    // refs and a dry remove on some unused functions (and a module)
    let unused = json(&(rs.run)(&["unused", "--json"]).0);
    let ids: Vec<String> =
        unused.as_array().map(|a| a.iter().take(4).filter_map(|u| u["id"].as_str().map(String::from)).collect()).unwrap_or_default();
    if !ids.is_empty() {
        let mut names: Vec<String> = ids.iter().map(|i| i.rsplit('.').next().unwrap_or(i).to_string()).collect();
        names.push(ids[0].rsplit_once('.').map(|x| x.0.to_string()).unwrap_or_default());
        let mut args: Vec<&str> = vec!["refs", "--json"];
        args.extend(names.iter().map(|s| s.as_str()));
        let ((p, pms), (r, rms)) = ((py.run)(&args), (rs.run)(&args));
        row("refs --json", diff_values("refs", &Value::Array(json_stream(&p)), &Value::Array(json_stream(&r))), pms, rms);
        args[1] = "--limit";
        args.insert(2, "5");
        let ((p, pms), (r, rms)) = ((py.run)(&args), (rs.run)(&args));
        row("refs (text)", diff_text("refs", &p, &r, false), pms, rms);
        let mut args: Vec<&str> = vec!["remove"];
        args.extend(ids.iter().take(3).map(|s| s.as_str()));
        let ((p, pms), (r, rms)) = ((py.run)(&args), (rs.run)(&args));
        row("remove (dry)", diff_text("remove", &p, &r, false), pms, rms);
    }
    // text reports
    for (cmd, bag) in [("tests", false), ("test-only", false), ("unused", false), ("hotspots", false), ("dups", true), ("tidy", true)]
    {
        let args = [cmd, "--limit", "100000"];
        let ((p, pms), (r, rms)) = ((py.run)(&args), (rs.run)(&args));
        row(&format!("{cmd} (text)"), diff_text(cmd, &p, &r, bag), pms, rms);
    }
    for cmd in ["test-only", "unused", "tests"] {
        let ((p, pms), (r, rms)) = ((py.run)(&[cmd, "--ids"]), (rs.run)(&[cmd, "--ids"]));
        row(&format!("{cmd} --ids"), diff_text(cmd, &p, &r, false), pms, rms);
    }
    total
}
