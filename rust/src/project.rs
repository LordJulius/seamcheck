//! The loaded project: config, files, and each file's text in the three
//! modes rules read (raw, nocomment, code), read and blanked once per run.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use crate::config::{self, Config};
use crate::elixir;
use crate::scan::blank_text;
use crate::walk;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Mode {
    Raw,
    NoComment,
    Code,
}

impl Mode {
    pub fn parse(s: &str) -> Mode {
        match s {
            "raw" => Mode::Raw,
            "nocomment" => Mode::NoComment,
            _ => Mode::Code,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: String,
    pub severity: String,
    pub file: String,
    pub line: usize,
    pub func: String,
    pub message: String,
    pub snippet: String,
}

impl Finding {
    pub fn new(rule: &str, severity: &str, file: &str, line: usize, func: &str, message: String) -> Finding {
        Finding {
            rule: rule.into(),
            severity: severity.into(),
            file: file.into(),
            line,
            func: func.into(),
            message,
            snippet: String::new(),
        }
    }
    pub fn snip(mut self, s: String) -> Finding {
        self.snippet = s;
        self
    }
}

type Memo = Mutex<HashMap<String, Arc<dyn Any + Send + Sync>>>;

pub struct Project {
    pub root: PathBuf,
    pub config: Config,
    pub files: Vec<String>,
    pub file_set: HashSet<String>,
    pub config_problems: Vec<String>,
    /// gate: the files the change touched (rules narrow to them)
    pub gate_changed: Option<HashSet<String>>,
    raw: RwLock<HashMap<String, Arc<String>>>,
    texts: RwLock<HashMap<(String, Mode), Arc<Vec<String>>>>,
    ex: RwLock<HashMap<String, Arc<Vec<elixir::Module>>>>,
    all_modules: OnceLock<Vec<String>>,
    memo: Memo,
}

/// Brace-language extensions and their scan family (lang_other.LANGS).
pub fn brace_family(ext: &str) -> Option<&'static str> {
    Some(match ext {
        ".js" | ".jsx" | ".mjs" | ".cjs" | ".ts" | ".tsx" | ".mts" | ".qml" => "js",
        ".rs" => "rust",
        ".kt" | ".kts" => "kotlin",
        ".java" | ".cs" | ".go" | ".c" | ".cpp" | ".cc" | ".cxx" | ".hpp" | ".hh" | ".h" => "c",
        ".swift" => "swift",
        ".php" => "php",
        _ => return None,
    })
}

pub fn ext_of(rel: &str) -> &str {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    match base.rfind('.') {
        Some(0) | None => "",
        Some(k) => &base[k..],
    }
}

/// core.scan_family
pub fn scan_family(rel: &str) -> &'static str {
    let ext = ext_of(rel);
    match ext {
        ".gd" => "gdscript",
        ".ex" | ".exs" => "elixir",
        ".py" => "python",
        ".rb" | ".rake" | ".ru" => "ruby",
        _ => brace_family(ext).unwrap_or("python"),
    }
}

/// Does seamcheck parse this file into modules (core.adapter_for)?
pub fn has_adapter(rel: &str) -> bool {
    let ext = ext_of(rel);
    matches!(ext, ".gd" | ".ex" | ".exs" | ".py" | ".rb" | ".rake" | ".ru") || brace_family(ext).is_some()
}

pub fn is_elixir(rel: &str) -> bool {
    matches!(ext_of(rel), ".ex" | ".exs")
}

/// Python's text-mode read: undecodable bytes replaced, newlines universal.
pub fn read_text(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let s = String::from_utf8_lossy(&bytes);
    Some(if s.contains('\r') { s.replace("\r\n", "\n").replace('\r', "\n") } else { s.into_owned() })
}

impl Project {
    pub fn load(root: &Path, extra: &[String]) -> Project {
        let (config, problems) = config::load(root, extra);
        let proj = config.table("project");
        let include = config::list_or(proj, "include", &["**/*"]);
        let mut exclude = walk::default_exclude(&include);
        exclude.extend(config::list_or(proj, "exclude", &[]));
        let files = walk::list_files(root, &include, &exclude);
        Project::with_files(root, config, problems, files)
    }

    pub fn with_files(root: &Path, config: Config, problems: Vec<String>, files: Vec<String>) -> Project {
        Project {
            root: root.to_path_buf(),
            config,
            file_set: files.iter().cloned().collect(),
            files,
            config_problems: problems,
            gate_changed: None,
            raw: RwLock::new(HashMap::new()),
            texts: RwLock::new(HashMap::new()),
            ex: RwLock::new(HashMap::new()),
            all_modules: OnceLock::new(),
            memo: Mutex::new(HashMap::new()),
        }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// The file's text (Project.text).
    pub fn text(&self, rel: &str) -> Arc<String> {
        if let Some(t) = self.raw.read().unwrap().get(rel) {
            return t.clone();
        }
        let t = Arc::new(read_text(&self.path(rel)).unwrap_or_default());
        self.raw.write().unwrap().insert(rel.to_string(), t.clone());
        t
    }

    /// A file's lines in a mode (core._text_for).
    pub fn text_for(&self, rel: &str, mode: Mode) -> Arc<Vec<String>> {
        let key = (rel.to_string(), mode);
        if let Some(t) = self.texts.read().unwrap().get(&key) {
            return t.clone();
        }
        let raw = self.text(rel);
        let out: Vec<String> = match mode {
            Mode::Raw => raw.split('\n').map(String::from).collect(),
            Mode::NoComment => blank_text(&raw, scan_family(rel), true).split('\n').map(String::from).collect(),
            // a parsed file's code is its module's (adapter tweaks included); a file
            // outside the project (a repo-wide rule's script) is plainly blanked
            Mode::Code if self.file_set.contains(rel) && has_adapter(rel) => {
                code_text(rel, &raw).split('\n').map(String::from).collect()
            }
            Mode::Code => blank_text(&raw, scan_family(rel), false).split('\n').map(String::from).collect(),
        };
        let out = Arc::new(out);
        self.texts.write().unwrap().insert(key, out.clone());
        out
    }

    /// Whole-text form of `text_for` (joined with newlines).
    pub fn joined(&self, rel: &str, mode: Mode) -> String {
        self.text_for(rel, mode).join("\n")
    }

    /// Do we know this file's functions in phase 1? (Elixir parsed, or no adapter at all.)
    pub fn funcs_known(&self, rel: &str) -> bool {
        is_elixir(rel) || !has_adapter(rel)
    }

    /// The file's modules (project.by_file): Elixir only in phase 1.
    pub fn modules(&self, rel: &str) -> Arc<Vec<elixir::Module>> {
        if !is_elixir(rel) || !self.file_set.contains(rel) {
            return Arc::new(Vec::new());
        }
        if let Some(m) = self.ex.read().unwrap().get(rel) {
            return m.clone();
        }
        let raw = self.text(rel);
        let code = self.joined(rel, Mode::Code);
        let mods = Arc::new(elixir::parse(rel, &raw, &code));
        self.ex.write().unwrap().insert(rel.to_string(), mods.clone());
        mods
    }

    /// The innermost real function around a line (core._enclosing).
    pub fn enclosing(&self, rel: &str, line: usize) -> Option<elixir::Func> {
        let mods = self.modules(rel);
        let inside: Vec<&elixir::Func> =
            mods.iter().flat_map(|m| m.funcs.iter()).filter(|f| f.start <= line && line <= f.end).collect();
        let real: Vec<&&elixir::Func> = inside.iter().filter(|f| f.kind != "top").collect();
        if !real.is_empty() {
            // max by (start, -end): the latest start, then the shortest; first such in order
            let mut best = real[0];
            for f in &real[1..] {
                if (f.start, std::cmp::Reverse(f.end)) > (best.start, std::cmp::Reverse(best.end)) {
                    best = f;
                }
            }
            return Some((**best).clone());
        }
        inside.first().map(|f| (*f).clone())
    }

    /// The function holding a line as forbid/together see it (the last module's func_at wins).
    pub fn func_at(&self, rel: &str, line: usize) -> Option<elixir::Func> {
        let mut f = None;
        for m in self.modules(rel).iter() {
            if let Some(g) = m.func_at(line) {
                f = Some(g.clone());
            }
        }
        f
    }

    /// Every module name the project defines (Elixir in phase 1), with
    /// `name@file` for a second definition of one name (core.load).
    pub fn module_names(&self) -> &Vec<String> {
        if let Some(v) = self.all_modules.get() {
            return v;
        }
        // computed outside the cell: a rayon worker that steals another rule
        // asking for the names mid-computation must not wait on itself
        // (two first callers may both compute; the files' parses are cached)
        use rayon::prelude::*;
        let ex: Vec<&String> = self.files.iter().filter(|f| is_elixir(f)).collect();
        let per: Vec<Arc<Vec<elixir::Module>>> = ex.par_iter().map(|f| self.modules(f)).collect();
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        for (rel, mods) in ex.iter().zip(per) {
            for m in mods.iter() {
                let name = if seen.contains(&m.name) { format!("{}@{}", m.name, rel) } else { m.name.clone() };
                seen.insert(name.clone());
                out.push(name);
            }
        }
        let _ = self.all_modules.set(out);
        self.all_modules.get().unwrap()
    }

    /// A value computed once per run under a key (brace trees, the CSS index...).
    pub fn memo<T: Any + Send + Sync>(&self, key: &str, f: impl FnOnce() -> T) -> Arc<T> {
        if let Some(v) = self.memo.lock().unwrap().get(key) {
            if let Ok(t) = v.clone().downcast::<T>() {
                return t;
            }
        }
        let v = Arc::new(f());
        self.memo.lock().unwrap().insert(key.to_string(), v.clone());
        v
    }

    pub fn exclude(&self) -> Vec<String> {
        config::list_or(self.config.table("project"), "exclude", &[])
    }
}

/// Module.code: the blanked text, with what the brace adapter also does
/// (C# preprocessor lines, Kotlin/Swift backtick identifiers).
pub fn code_text(rel: &str, raw: &str) -> String {
    let mut code = blank_text(raw, scan_family(rel), false);
    let ext = ext_of(rel);
    if ext == ".cs" {
        static CS: OnceLock<crate::pyre::Rx> = OnceLock::new();
        let rx = CS.get_or_init(|| {
            crate::pyre::Rx::lit(r"(?m)^[ \t]*#[ \t]*(?:pragma|region|endregion|nullable|if|elif|else|endif|define|undef|warning|error|line)\b[^\n]*")
        });
        code = rx.sub_with(&code, |m| " ".repeat(m.group0().len()));
    }
    if matches!(ext, ".swift" | ".kt" | ".kts") {
        static BT: OnceLock<crate::pyre::Rx> = OnceLock::new();
        static NW: OnceLock<regex::Regex> = OnceLock::new();
        let rx = BT.get_or_init(|| crate::pyre::Rx::lit(r"`([^`\n]+)`"));
        let nw = NW.get_or_init(|| regex::Regex::new(r"\W").unwrap());
        code = rx.sub_with(&code, |m| format!(" {} ", nw.replace_all(m.s(1), "_")));
    }
    code
}
