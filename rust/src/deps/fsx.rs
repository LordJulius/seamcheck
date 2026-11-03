//! The file system as deps.py sees it: `os.walk` order (directory entries as
//! the kernel lists them, depth first, symlinked folders listed but never
//! entered), `glob.glob` order and hidden-name rules, `os.path` arithmetic,
//! and line numbers of byte offsets.

use std::path::{Path, PathBuf};

use crate::glob::fnmatchcase;

/// A file's text (`open(errors="replace")`, universal newlines), "" when unreadable.
pub fn read(path: &Path) -> String {
    crate::project::read_text(path).unwrap_or_default()
}

/// One directory's entries in listing order: (name, is a directory (links
/// followed), is a symlink).
pub fn scandir(d: &Path) -> Vec<(String, bool, bool)> {
    let Ok(rd) = std::fs::read_dir(d) else { return Vec::new() };
    rd.flatten()
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let link = e.file_type().map(|t| t.is_symlink()).unwrap_or(false);
            let dir = if link {
                std::fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false)
            } else {
                e.file_type().map(|t| t.is_dir()).unwrap_or(false)
            };
            (name, dir, link)
        })
        .collect()
}

/// `os.walk(root/top)` top-down: each directory's (relative path, kept
/// sub-directories, files); `keep` prunes sub-directories (`dirs[:] = ...`).
pub fn walk(root: &Path, top: &str, mut keep: impl FnMut(&str, &str) -> bool, mut each: impl FnMut(&str, &[String])) {
    let mut stack = vec![top.to_string()];
    while let Some(dir) = stack.pop() {
        let mut dirs: Vec<(String, bool)> = Vec::new();
        let mut files: Vec<String> = Vec::new();
        for (name, is_dir, link) in scandir(&root.join(&dir)) {
            if is_dir {
                dirs.push((name, link));
            } else {
                files.push(name);
            }
        }
        each(&dir, &files);
        let kept: Vec<(String, bool)> = dirs.into_iter().filter(|(x, _)| keep(&dir, x)).collect();
        for (x, link) in kept.into_iter().rev() {
            if !link {
                stack.push(join(&dir, &x));
            }
        }
    }
}

/// `os.path.join(a, b)` for relative paths ("" is the root).
pub fn join(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else if a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

/// `os.path.dirname`.
pub fn dirname(p: &str) -> &str {
    match p.rfind('/') {
        Some(0) => "/",
        Some(i) => p[..i].trim_end_matches('/'),
        None => "",
    }
}

/// `os.path.basename`.
pub fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// `os.path.normpath` of a relative or absolute path.
pub fn normpath(p: &str) -> String {
    let abs = p.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." if out.last().is_some_and(|l| *l != "..") => {
                out.pop();
            }
            ".." if abs => {}
            x => out.push(x),
        }
    }
    let body = out.join("/");
    match (abs, body.is_empty()) {
        (true, _) => format!("/{body}"),
        (false, true) => ".".into(),
        (false, false) => body,
    }
}

/// `os.path.relpath(p, start)` for two paths relative to one directory.
pub fn relpath(p: &str, start: &str) -> String {
    let a = normpath(p);
    let b = normpath(start);
    let pa: Vec<&str> = a.split('/').filter(|x| *x != ".").collect();
    let pb: Vec<&str> = b.split('/').filter(|x| *x != ".").collect();
    let common = pa.iter().zip(&pb).take_while(|(x, y)| x == y).count();
    let mut out: Vec<&str> = vec![".."; pb.len() - common];
    out.extend(&pa[common..]);
    if out.is_empty() {
        ".".into()
    } else {
        out.join("/")
    }
}

fn magic(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

fn split(p: &str) -> (&str, &str) {
    match p.rfind('/') {
        Some(i) => {
            let head = &p[..=i];
            let head = if head.trim_end_matches('/').is_empty() { head } else { head.trim_end_matches('/') };
            (head, &p[i + 1..])
        }
        None => ("", p),
    }
}

thread_local! {
    static LISTINGS: std::cell::RefCell<Option<std::collections::HashMap<String, Vec<(String, bool, bool)>>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f` with glob's directory listings kept (one analysis: the cargo
/// registry is listed once, not once per dependency).
pub fn with_listings<T>(f: impl FnOnce() -> T) -> T {
    LISTINGS.with(|l| *l.borrow_mut() = Some(Default::default()));
    let out = f();
    LISTINGS.with(|l| *l.borrow_mut() = None);
    out
}

fn listdir(d: &str, dironly: bool) -> Vec<String> {
    let dir = if d.is_empty() { Path::new(".") } else { Path::new(d) };
    let cached = LISTINGS.with(|l| {
        let mut l = l.borrow_mut();
        l.as_mut().map(|m| m.entry(d.to_string()).or_insert_with(|| scandir(dir)).clone())
    });
    cached
        .unwrap_or_else(|| scandir(dir))
        .into_iter()
        .filter(|(_, is_dir, _)| !dironly || *is_dir)
        .map(|(n, _, _)| n)
        .collect()
}

fn hidden(name: &str) -> bool {
    name.starts_with('.')
}

fn glob1(d: &str, pat: &str, dironly: bool) -> Vec<String> {
    listdir(d, dironly).into_iter().filter(|n| (hidden(pat) || !hidden(n)) && fnmatchcase(n, pat)).collect()
}

fn rlistdir(d: &str, dironly: bool, out: &mut Vec<String>, prefix: &str) {
    for x in listdir(d, dironly) {
        if hidden(&x) {
            continue;
        }
        let rel = join(prefix, &x);
        out.push(rel.clone());
        rlistdir(&join(d, &x), dironly, out, &rel);
    }
}

fn iglob(pathname: &str, dironly: bool) -> Vec<String> {
    let (dirname, base) = split(pathname);
    if !magic(pathname) {
        let ok =
            if base.is_empty() { Path::new(dirname).is_dir() } else { std::fs::symlink_metadata(pathname).is_ok() };
        return if ok { vec![pathname.to_string()] } else { Vec::new() };
    }
    let dirs: Vec<String> =
        if dirname != pathname && magic(dirname) { iglob(dirname, true) } else { vec![dirname.to_string()] };
    let mut out = Vec::new();
    for d in dirs {
        let names: Vec<String> = if !magic(base) {
            let p = join(&d, base);
            let ok = if base.is_empty() { Path::new(&d).is_dir() } else { std::fs::symlink_metadata(&p).is_ok() };
            if ok {
                vec![base.to_string()]
            } else {
                Vec::new()
            }
        } else if base == "**" {
            let mut v = vec![String::new()];
            rlistdir(&d, dironly, &mut v, "");
            v
        } else {
            glob1(&d, base, dironly)
        };
        for n in names {
            out.push(join(&d, &n));
        }
    }
    out
}

/// `glob.glob(pattern, recursive=True)` in Python's order.
pub fn glob(pattern: &str) -> Vec<String> {
    iglob(pattern, false)
}

/// pos -> 1-based line, for many lookups in one text (`_liner`).
pub struct Liner(Vec<usize>);

impl Liner {
    pub fn new(text: &str) -> Liner {
        Liner(text.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| i).collect())
    }
    pub fn line(&self, pos: usize) -> usize {
        self.0.partition_point(|&i| i < pos) + 1
    }
}

/// `text.count("\n", 0, pos) + 1`.
pub fn line_of(text: &str, pos: usize) -> usize {
    text.as_bytes()[..pos.min(text.len())].iter().filter(|b| **b == b'\n').count() + 1
}

/// The main working tree when `root` is a linked git worktree, else "" (`_main_checkout`).
pub fn main_checkout(root: &Path) -> String {
    let git = |args: &[&str]| -> String {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    let top = git(&["rev-parse", "--show-toplevel"]);
    if !common.is_empty() && basename(&common) == ".git" {
        let cand = dirname(&common).to_string();
        let real = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
        if !top.is_empty() && real(&cand) != real(&top) {
            return cand;
        }
    }
    String::new()
}

fn nonempty_dir(p: &Path) -> bool {
    std::fs::read_dir(p).map(|mut r| r.next().is_some()).unwrap_or(false)
}

/// The folder holding a manifest's installed packages (`_installed`): its
/// own, else the main checkout's for the same manifest, else None.
pub fn installed(root: &Path, main: &str, mdir_rel: &str, sub: &str) -> Option<String> {
    let own = root.join(mdir_rel).join(sub);
    if nonempty_dir(&own) {
        return Some(abs_join(root, &join(mdir_rel, sub)));
    }
    if !main.is_empty() {
        let other = join(&join(main, if mdir_rel.is_empty() { "." } else { mdir_rel }), sub);
        if nonempty_dir(Path::new(&other)) {
            return Some(other);
        }
    }
    None
}

/// `os.path.join(root, rel)` as a string ("" = the root itself).
pub fn abs_join(root: &Path, rel: &str) -> String {
    let r = root.to_string_lossy();
    if rel.is_empty() {
        r.into_owned()
    } else {
        join(&r, rel)
    }
}

/// `os.path.expanduser("~")`.
pub fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}
