//! The gate's exactness guard (exactness.py): a change that quietly breaks
//! import resolution shows up.
//!
//! When a change removes or breaks an import seamcheck follows, the calls
//! through it fall back to name guesses. So each gate on a tree that is
//! HEAD's records, per language, how many calls into project code are exact
//! (`Graph::shares`), and the gate compares the languages a change touches
//! against that record. A drop of more than DROP points warns, never blocks.
//!
//! Records live in the shared cache (`~/.cache/seamcheck`, one small JSON
//! file per repository, apart from the Python package's pickle), keyed by
//! commit, this binary and the config.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use indexmap::IndexMap;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};

use crate::graph::Graph;
use crate::project::{ext_of, has_adapter, Project};

/// percentage points of "project calls exact" a change may lose quietly
pub const DROP: f64 = 2.0;
/// Measuring a commit no run recorded loads its files from scratch; only a
/// project this small does that within a gate's budget.
pub const CHEAP_FILES: usize = 300;
const KEEP: usize = 64;

pub const WHY: &str = "calls that lost their import fall back to name guesses: rules and queries over them see less";
pub const FIX: &str = "check the imports this change removed or moved (seamcheck stats shows the shares)";

pub type Shares = IndexMap<String, (i64, i64)>;

/// extension -> (exact, name guess) call sites into project code, the
/// languages with any (exactness.shares).
pub fn shares(g: &Graph) -> Shares {
    g.resolution.iter().filter(|(_, r)| r[0] + r[1] != 0).map(|(k, r)| (k.clone(), (r[0] as i64, r[1] as i64))).collect()
}

/// One language whose exact share fell.
#[derive(Debug, Clone, PartialEq)]
pub struct Drop {
    pub ext: String,
    pub language: String,
    pub before: f64,
    pub after: f64,
    pub calls_before: (i64, i64),
    pub calls_after: (i64, i64),
    pub file: String,
}

impl Drop {
    pub fn json(&self) -> Value {
        json!({"ext": self.ext, "language": self.language, "before": self.before, "after": self.after,
               "calls_before": [self.calls_before.0, self.calls_before.1],
               "calls_after": [self.calls_after.0, self.calls_after.1], "file": self.file})
    }

    pub fn message(&self, r: &str) -> String {
        format!(
            "{} ({}) calls into project code exact: {}% at {r}, {}% now ({} of {} -> {} of {})",
            self.language,
            self.ext,
            pyfloat(self.before),
            pyfloat(self.after),
            self.calls_before.0,
            self.calls_before.0 + self.calls_before.1,
            self.calls_after.0,
            self.calls_after.0 + self.calls_after.1
        )
    }
}

/// A float as Python's str() prints it (`100.0`, `66.7`).
pub fn pyfloat(x: f64) -> String {
    let s = format!("{x}");
    if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
        s
    } else {
        format!("{s}.0")
    }
}

/// init._LANG
fn language(ext: &str) -> String {
    match ext {
        ".ex" | ".exs" => "Elixir",
        ".kt" | ".kts" => "Kotlin",
        ".java" => "Java",
        ".ts" | ".tsx" => "TypeScript",
        ".js" | ".jsx" | ".mjs" => "JavaScript",
        ".py" => "Python",
        ".rs" => "Rust",
        ".go" => "Go",
        ".cs" => "C#",
        ".c" => "C",
        ".cpp" | ".cc" | ".h" | ".hpp" => "C++",
        ".qml" => "QML",
        ".gd" => "GDScript",
        ".swift" => "Swift",
        ".php" => "PHP",
        other => other,
    }
    .to_string()
}

fn pct(c: (i64, i64)) -> f64 {
    100.0 * c.0 as f64 / (c.0 + c.1) as f64
}

fn round1(x: f64) -> f64 {
    format!("{x:.1}").parse().unwrap_or(x)
}

/// Where the records live, and how big a project may be measured from scratch.
pub struct Book {
    pub dir: PathBuf,
    pub cheap_files: usize,
}

impl Default for Book {
    fn default() -> Book {
        let dir = std::env::var_os("SEAMCHECK_CACHE_DIR").map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache").join("seamcheck")
        });
        Book { dir, cheap_files: CHEAP_FILES }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sha1_hex(s: &[u8]) -> String {
    hex(&Sha1::digest(s))
}

/// One key for every checkout of a repository: git's common dir (fingerprint.repo_key).
pub fn repo_key(root: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| crate::pyre::strip(&String::from_utf8_lossy(&o.stdout)).to_string())
        .filter(|s| !s.is_empty());
    let base = out.unwrap_or_else(|| std::path::absolute(root).unwrap_or_else(|_| root.into()).to_string_lossy().to_string());
    sha1_hex(format!("repo|{base}").as_bytes())[..16].to_string()
}

fn commit(root: &Path, r: &str) -> String {
    Command::new("git")
        .args(["rev-parse", "--verify", "-q", &format!("{r}^{{commit}}")])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| crate::pyre::strip(&String::from_utf8_lossy(&o.stdout)).to_string())
        .unwrap_or_default()
}

/// This binary: a rebuilt seamcheck may measure differently and must not read as a regression.
fn code_version() -> String {
    let stamp = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| format!("{}:{:?}", m.len(), m.modified().ok()))
        .unwrap_or_default();
    format!("{}-{}", crate::VERSION, &sha1_hex(stamp.as_bytes())[..12])
}

fn key(p: &Project, sha: &str) -> String {
    let cfg = serde_json::to_string(&sorted(&Value::Object(p.config.v.clone()))).unwrap_or_default();
    format!("{sha}|{}|{}", code_version(), &sha1_hex(cfg.as_bytes())[..12])
}

fn sorted(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort();
            Value::Object(keys.into_iter().map(|k| (k.clone(), sorted(&o[k]))).collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
        x => x.clone(),
    }
}

impl Book {
    fn path(&self, root: &Path) -> PathBuf {
        self.dir.join(format!("{}-exactness-rs.json", repo_key(root)))
    }

    fn load(&self, root: &Path) -> IndexMap<String, Shares> {
        let Ok(text) = std::fs::read_to_string(self.path(root)) else { return IndexMap::new() };
        let Ok(Value::Object(o)) = serde_json::from_str::<Value>(&text) else { return IndexMap::new() };
        o.into_iter()
            .map(|(k, v)| {
                let sh: Shares = v
                    .as_object()
                    .map(|m| {
                        m.iter()
                            .map(|(e, c)| (e.clone(), (c[0].as_i64().unwrap_or(0), c[1].as_i64().unwrap_or(0))))
                            .collect()
                    })
                    .unwrap_or_default();
                (k, sh)
            })
            .collect()
    }

    /// Atomic: two lanes saving at once leave one complete file.
    fn save(&self, p: &Project, sha: &str, value: &Shares) {
        let mut book = self.load(&p.root);
        let k = key(p, sha);
        book.shift_remove(&k);
        book.insert(k, value.clone()); // (last in, newest: the oldest go first)
        let skip = book.len().saturating_sub(KEEP);
        let out: serde_json::Map<String, Value> = book
            .into_iter()
            .skip(skip)
            .map(|(k, v)| (k, Value::Object(v.into_iter().map(|(e, c)| (e, json!([c.0, c.1]))).collect())))
            .collect();
        let path = self.path(&p.root);
        let _ = std::fs::create_dir_all(&self.dir);
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, Value::Object(out).to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Would a change to this file change the graph? A code file the project
    /// parses (or parsed: a deletion), or its seamcheck config.
    fn in_graph(p: &Project, g: &Graph, rel: &str) -> bool {
        if rel == "seamcheck.toml" || rel.starts_with("seamcheck.d/") {
            return true;
        }
        let pc = p.config.table("project");
        let include = crate::config::list_or(pc, "include", &["**/*"]);
        let mut exclude = crate::walk::default_exclude(&include);
        exclude.extend(crate::config::list_or(pc, "exclude", &[]));
        g.by_file.contains_key(rel)
            || (has_adapter(rel) && crate::glob::match_any(rel, &include) && !crate::glob::match_any(rel, &exclude))
    }

    /// Remember this tree's shares as HEAD's, when the graph is HEAD's (no
    /// parsed file or config differs from it). The graph loads only when a
    /// record is due.
    pub fn record<'g>(&self, p: &Project, g: &dyn Fn() -> &'g Graph, changed: Option<&std::collections::BTreeSet<String>>) {
        let own;
        let changed = match changed {
            Some(c) => c,
            None => {
                own = crate::tidy::changed_files(p, "HEAD");
                &own
            }
        };
        // (a parse-free look first: a changed code file or config means no record)
        if changed.iter().any(|rel| rel == "seamcheck.toml" || rel.starts_with("seamcheck.d/") || has_adapter(rel) && p.file_set.contains(rel)) {
            return;
        }
        let sha = commit(&p.root, "HEAD");
        if sha.is_empty() || self.load(&p.root).contains_key(&key(p, &sha)) {
            return; // (recorded already: the same commit, binary and config measure the same)
        }
        let g = g();
        if changed.iter().any(|rel| Book::in_graph(p, g, rel)) {
            return;
        }
        self.save(p, &sha, &shares(g));
    }

    fn cheap(&self, p: &Project, g: &Graph) -> bool {
        // a bridge's `run` (mix phx.routes) would run again in the copy
        let runs = p.config.list("bridge").iter().any(|b| {
            ["from", "to"].iter().any(|side| b.get(*side).and_then(|s| s.as_object()).is_some_and(|s| s.contains_key("run")))
        });
        g.by_file.len() <= self.cheap_files && !runs
    }

    /// The commit's files in a scratch dir under today's config: what the
    /// change did to resolution, not what a config edit did.
    fn measure(&self, p: &Project, sha: &str, extra: &[String]) -> Option<Shares> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let tmp = std::env::temp_dir().join(format!("seamcheck-head-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&tmp).ok()?;
        let got = (|| {
            let mut arch = Command::new("git")
                .args(["archive", "--format=tar", sha])
                .current_dir(&p.root)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            let tar = Command::new("tar").arg("-x").arg("-C").arg(&tmp).stdin(arch.stdout.take()?).status().ok()?;
            if !arch.wait().ok()?.success() || !tar.success() {
                return None;
            }
            let _ = std::fs::remove_file(tmp.join("seamcheck.toml"));
            if let Ok(rd) = std::fs::read_dir(tmp.join("seamcheck.d")) {
                for e in rd.flatten() {
                    let _ = std::fs::remove_file(e.path());
                }
            }
            let cfgs: Vec<String> =
                crate::config::files_for(&p.root, extra).iter().map(|x| x.to_string_lossy().to_string()).collect();
            let then = Project::load(&tmp, &cfgs);
            Some(shares(&Graph::load_scratch(&then, false)))
        })();
        let _ = std::fs::remove_dir_all(&tmp);
        got
    }

    /// The shares recorded for `r`'s commit, measured now when no run
    /// recorded them and that is cheap; None when unknown.
    pub fn baseline(&self, p: &Project, g: &Graph, r: &str, extra: &[String]) -> Option<Shares> {
        let sha = commit(&p.root, r);
        if sha.is_empty() {
            return None;
        }
        let mut got = self.load(&p.root).shift_remove(&key(p, &sha));
        if got.is_none() && self.cheap(p, g) {
            got = self.measure(p, &sha, extra);
            if let Some(v) = &got {
                self.save(p, &sha, v);
            }
        }
        got
    }

    /// The languages this change touches whose exact share fell more than
    /// DROP points since `r`. Records the tree first when it is HEAD's.
    pub fn drops<'g>(
        &self,
        p: &Project,
        g: &dyn Fn() -> &'g Graph,
        r: &str,
        changed: &std::collections::BTreeSet<String>,
        extra: &[String],
    ) -> Vec<Drop> {
        self.record(p, g, None);
        let touched: Vec<&String> = changed.iter().filter(|rel| has_adapter(rel)).collect();
        if touched.is_empty() {
            return vec![];
        }
        let gr = g();
        let mut exts: Vec<&str> = touched.iter().filter(|rel| Book::in_graph(p, gr, rel)).map(|rel| ext_of(rel)).collect();
        exts.sort();
        exts.dedup();
        if exts.is_empty() {
            return vec![];
        }
        let Some(before) = self.baseline(p, gr, r, extra).filter(|b| !b.is_empty()) else { return vec![] };
        let now = shares(gr);
        let mut out = Vec::new();
        for ext in exts {
            let (Some(b), Some(a)) = (before.get(ext), now.get(ext)) else { continue };
            let (pb, pa) = (pct(*b), pct(*a));
            if pb - pa > DROP {
                let first = changed.iter().find(|rel| rel.ends_with(ext)).cloned().unwrap_or_default();
                out.push(Drop {
                    ext: ext.to_string(),
                    language: language(ext),
                    before: round1(pb),
                    after: round1(pa),
                    calls_before: *b,
                    calls_after: *a,
                    file: first,
                });
            }
        }
        out
    }
}
