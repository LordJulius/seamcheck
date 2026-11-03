//! Removing code (tidy.remove): functions or test blocks by id, with the doc
//! comments / @doc heredocs / annotations right above each, and (cascading)
//! the helpers and file-level declarations only the removed code used.

use std::collections::{BTreeSet, HashMap, HashSet};

use indexmap::IndexMap;

use super::{Corpus, Tidy};
use crate::model::Func;
use crate::project::{ext_of, read_text};
use crate::pyre::{strip, Rx};

macro_rules! rx {
    ($pat:expr) => {{
        static R: std::sync::OnceLock<Rx> = std::sync::OnceLock::new();
        R.get_or_init(|| Rx::lit($pat))
    }};
}

/// Visible only inside its file: Elixir defp, JS/TS not exported, Kotlin /
/// Java / C# `private`, Python `_name`.
fn is_private(t: &Tidy, f: &Func) -> bool {
    if f.kind == "private" {
        return true;
    }
    let line = t.module(f).and_then(|m| m.raw.get(f.start.wrapping_sub(1))).map(|s| s.as_str()).unwrap_or("");
    match ext_of(&f.file) {
        ".ts" | ".tsx" | ".js" | ".jsx" | ".mjs" | ".mts" => !line.contains("export"),
        ".kt" | ".kts" | ".java" | ".cs" => rx!(r"\bprivate\b").search(line).is_some(),
        ".py" | ".pyw" => f.name.starts_with('_') && !f.name.starts_with("__"),
        _ => false,
    }
}

/// Functions left with no caller once `gone` is removed, repeatedly: every
/// graph caller is gone, it isn't an entry point, no other file names it, and
/// every mention in its own file sits inside something removed.
fn orphans(t: &Tidy, gone: &HashSet<String>) -> Vec<String> {
    let g = t.g;
    let corpus = Corpus::new(t.p);
    let mut gone = gone.clone();
    let mut out = Vec::new();
    let mut changed = true;
    while changed {
        changed = false;
        let mut spans: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        for id in &gone {
            let f = g.func(id).unwrap();
            spans.entry(f.file.clone()).or_default().push((f.start, f.end));
        }
        for f in t.funcs() {
            if gone.contains(&f.id) || !(f.kind == "func" || f.kind == "private") || t.is_entry(f) {
                continue;
            }
            let cs: Vec<&String> =
                g.redges.get(&f.id).map(|v| v.iter().map(|(c, _, _)| c).filter(|c| g.funcs.contains_key(*c)).collect()).unwrap_or_default();
            // a file-private helper can't be used from another file, whatever
            // other files happen to name things; a public one can
            if cs.iter().any(|c| !gone.contains(*c))
                || (!is_private(t, f) && !corpus.elsewhere(&f.name, &f.file, |_| false).is_empty())
            {
                continue;
            }
            let raw: &[String] = t.module(f).map(|m| m.raw.as_slice()).unwrap_or(&[]);
            let pat = Rx::lit(&format!(r"(?<![\w]){}(?![\w?!])", regex::escape(&f.name)));
            let mentions: Vec<usize> = raw
                .iter()
                .enumerate()
                .filter(|(i, ln)| pat.search(ln).is_some() && !(f.start <= i + 1 && i + 1 <= f.end))
                .map(|(i, _)| i + 1)
                .collect();
            let sp = spans.get(&f.file).map(|v| v.as_slice()).unwrap_or(&[]);
            let inside = mentions.iter().filter(|i| sp.iter().any(|(a, b)| a <= i && *i <= b)).count();
            // used only from removed code (a call or a `&name(&1)` capture the
            // graph may not see); never-used code is `unused`'s job, not this
            if inside == 0 || inside != mentions.len() {
                continue;
            }
            gone.insert(f.id.clone());
            out.push(f.id.clone());
            changed = true;
        }
    }
    out
}

const BUILTIN_ATTRS: &[&str] = &[
    "doc", "moduledoc", "spec", "impl", "type", "typep", "opaque", "callback", "behaviour", "derive", "enforce_keys",
    "primary_key", "foreign_key_type", "schema_prefix", "timestamps_opts", "before_compile", "after_compile", "on_load",
    "compile", "dialyzer", "deprecated", "external_resource", "optional_callbacks", "vsn", "tag", "moduletag",
    "describetag", "macrocallback", "since",
];
const JS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".mts"];

/// The declaration form of a file's language, and how a use of a name is spelled.
fn decl_spec(rel: &str) -> Option<(&'static Rx, fn(&str) -> String)> {
    let ends = |xs: &[&str]| xs.iter().any(|x| rel.ends_with(x));
    if ends(&[".ex", ".exs"]) {
        Some((rx!(r"\s*@(\w+)\s+\S"), |n| format!(r"@{n}\b")))
    } else if ends(JS) {
        Some((rx!(r"(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*[:=]"), |n| format!(r"(?<![\w$.]){}(?![\w$])", regex::escape(n))))
    } else if ends(&[".kt", ".kts"]) {
        Some((rx!(r"\s*private\s+(?:const\s+)?val\s+(\w+)\b"), |n| format!(r"(?<![\w.]){n}\b")))
    } else if ends(&[".py", ".pyw"]) {
        Some((rx!(r"(_[A-Za-z]\w*)\s*="), |n| format!(r"(?<![\w.]){n}\b")))
    } else {
        None
    }
}

fn brackets(s: &str) -> i64 {
    s.chars().filter(|c| "[{(".contains(*c)).count() as i64 - s.chars().filter(|c| "]})".contains(*c)).count() as i64
}

/// Declarations (Elixir @attributes, top-level non-exported JS/TS consts,
/// Kotlin private vals, Python _names) whose every use sat in removed code:
/// their lines join `drop`. Repeats: one may feed another.
fn drop_orphan_decls(rel: &str, lines: &[String], drop: &mut BTreeSet<usize>) -> Vec<String> {
    let Some((decl, use_)) = decl_spec(rel) else { return vec![] };
    let mut report = Vec::new();
    let mut removed_text = drop.iter().filter_map(|k| lines.get(*k)).cloned().collect::<Vec<_>>().join("\n");
    loop {
        let kept: Vec<(usize, &String)> = lines.iter().enumerate().filter(|(k, _)| !drop.contains(k)).collect();
        let text = kept.iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>().join("\n");
        let mut found = false;
        for (k, l) in &kept {
            let Some(m) = decl.match_start(l) else { continue };
            let name = m.s(1).to_string();
            if (rel.ends_with(".ex") || rel.ends_with(".exs")) && BUILTIN_ATTRS.contains(&name.as_str()) {
                continue;
            }
            if JS.iter().any(|x| rel.ends_with(x))
                && (l.chars().next().is_some_and(|c| c.is_whitespace()) || l[..m.span(1).unwrap().0].contains("export"))
            {
                continue; // only file-level, non-exported
            }
            let u = Rx::lit(&use_(&name));
            if u.find_iter(&text).len() > 1 || u.search(&removed_text).is_none() {
                continue; // still used, or never used by the removed code (not ours to judge)
            }
            let (mut depth, mut j) = (0i64, *k);
            while j < lines.len() {
                // a heredoc / multi-line string opened here runs to its closer
                // (`@prompt """` ... `"""`); its brackets are text, not structure
                let q = ["\"\"\"", "'''", "`"].into_iter().find(|d| lines[j].matches(d).count() % 2 == 1);
                if let Some(q) = q {
                    j += 1;
                    while j < lines.len() && !lines[j].contains(q) {
                        j += 1;
                    }
                    if j >= lines.len() {
                        break;
                    }
                    let tail = lines[j].split_once(q).map(|x| x.1).unwrap_or("");
                    depth += brackets(tail);
                } else {
                    depth += brackets(&lines[j]);
                }
                let r = lines[j].trim_end();
                if depth <= 0 && ![",", "=", "\\", "(", "[", "{"].iter().any(|e| r.ends_with(e)) {
                    break;
                }
                j += 1;
            }
            drop.extend(*k..=j);
            if j + 1 < lines.len() && strip(&lines[j + 1]).is_empty() && *k > 0 && strip(&lines[k - 1]).is_empty() {
                drop.insert(j + 1);
            }
            removed_text.push('\n');
            removed_text.push_str(&super::slice(lines, *k, j + 1).join("\n"));
            report.push(format!("{rel}:{}-{}  {name}  (orphaned)", k + 1, j + 1));
            found = true;
            break;
        }
        if !found {
            return report;
        }
    }
}

/// Delete functions (or test blocks) by id with what is attached above
/// each; every clause of a multi-clause function goes. With `cascade`,
/// helpers only the removed code called go too. Returns what was (or would
/// be) removed; nothing is written when `dry`.
pub fn remove(t: &Tidy, ids: &[String], dry: bool, cascade: bool) -> Result<Vec<String>, String> {
    let g = t.g;
    let missing: Vec<&str> = ids.iter().filter(|i| !g.funcs.contains_key(*i)).map(|s| s.as_str()).collect();
    if !missing.is_empty() {
        return Err(format!("no such function: {} (ids as `seamcheck find` prints them)", missing.join(", ")));
    }
    let orph = if cascade { orphans(t, &ids.iter().cloned().collect()) } else { vec![] };
    let mut by_file: IndexMap<String, Vec<&Func>> = IndexMap::new();
    for fid in ids.iter().chain(orph.iter()) {
        let f = g.func(fid).unwrap();
        by_file.entry(f.file.clone()).or_default().push(f);
    }
    let orphan_set: HashSet<&String> = orph.iter().collect();
    let heredoc_open = rx!(r#"(@doc|""")\s*(~S)?"""\s*$|@doc\s+~?S?""""#);
    let attached = rx!(r#"\s*(@\w+|#|//|/\*|\*|--|"""|'''|"""\s*$)"#);
    let not_attached = rx!(r"(@moduledoc|#!|#\s*(include|define|if|pragma|import)\b)");
    let mut report = Vec::new();
    for (rel, fs) in &by_file {
        let path = t.p.path(rel);
        let text = read_text(&path).ok_or_else(|| format!("{rel}: can't read it"))?;
        let lines: Vec<String> = text.split('\n').map(String::from).collect();
        let mut drop: BTreeSet<usize> = BTreeSet::new();
        for f in fs {
            let (a, b) = (f.start as i64 - 1, f.end as i64 - 1);
            let mut i = a - 1;
            while i >= 0 {
                let s = strip(&lines[i as usize]);
                if s == "\"\"\"" || s == "'''" {
                    // the end of a heredoc doc: find its opener
                    let mut j = i - 1;
                    while j >= 0 && heredoc_open.search(&lines[j as usize]).is_none() {
                        j -= 1;
                    }
                    if j >= 0 && lines[j as usize].contains("@doc") {
                        i = j - 1;
                        continue;
                    }
                    break;
                }
                if !s.is_empty() && attached.match_start(&lines[i as usize]).is_some() && not_attached.match_start(s).is_none() {
                    i -= 1;
                    continue;
                }
                break;
            }
            let start = (i + 1) as usize;
            let b = b as usize;
            drop.extend(start..=b);
            if b + 1 < lines.len() && strip(&lines[b + 1]).is_empty() && (start == 0 || strip(&lines[start - 1]).is_empty()) {
                drop.insert(b + 1);
            }
            report.push(format!(
                "{rel}:{}-{}  {}{}",
                start + 1,
                b + 1,
                f.name,
                if orphan_set.contains(&f.id) { "  (orphaned)" } else { "" }
            ));
        }
        if cascade {
            report.extend(drop_orphan_decls(rel, &lines, &mut drop));
        }
        if !dry {
            let kept: Vec<&str> = lines.iter().enumerate().filter(|(k, _)| !drop.contains(k)).map(|(_, l)| l.as_str()).collect();
            std::fs::write(&path, kept.join("\n")).map_err(|e| format!("{rel}: {e}"))?;
        }
    }
    Ok(report)
}
