//! The linked graph kept between runs (core.load's graph cache, fingerprint.py).
//!
//! An agent runs seamcheck many times on a tree that hasn't changed, and
//! parsing and linking is most of a run. The whole linked graph is written
//! once, keyed by what it was made from, and read back when the key is the
//! same. As in Python's cache a file is known by its content hash (a stamp
//! map of mtime + size keeps a warm run to a stat per file), the key covers
//! the code that made the graph, the merged config, the file list and the
//! hashes of the files the graph reads, so a changed file, config or binary
//! never reads an old graph; and the entries of every checkout of one
//! repository (git's common dir) are shared, so a lane's fresh worktree
//! reuses the main checkout's graph.
//!
//! Own files in the shared directory (`SEAMCHECK_CACHE_DIR`, else
//! `~/.cache/seamcheck`): `<repo>-graph-rs-<key>.bin`, `<repo>-graphs-rs.json`
//! (the newest few) and `<key of root>-stamps-rs.bin`. Python's pickles are
//! never read, and these are never read by Python.
//!
//! Only the CLI turns it on ([`enable`]); the library and the tests build
//! every graph from scratch.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use super::Graph;
use crate::project::{ext_of, Project};

/// How many graphs of one repository are kept (the newest).
const KEEP: usize = 3;
const MAGIC: &[u8; 8] = b"SEAMGR01";

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// Turn the graph cache on for this process, in `dir`.
pub fn enable(dir: PathBuf) {
    let _ = DIR.set(dir);
}

/// The directory the shared caches live in (fingerprint.CACHE_DIR).
pub fn default_dir() -> PathBuf {
    std::env::var_os("SEAMCHECK_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache").join("seamcheck"))
}

// --- Arc'd line lists shared by a file's modules ----------------------------------

thread_local! {
    static SEEN: RefCell<HashMap<usize, u32>> = RefCell::new(HashMap::new());
    static POOL: RefCell<Vec<Arc<Vec<String>>>> = const { RefCell::new(Vec::new()) };
}

/// serde adapter for `Module::raw` / `code`: several modules of one file hold
/// the same lines; written once, read back shared (not as a copy each).
pub mod shared_lines {
    use super::*;
    use serde::{Deserializer, Serializer};

    #[derive(Serialize)]
    enum Out<'a> {
        New(&'a Vec<String>),
        Same(u32),
    }

    #[derive(Deserialize)]
    enum In {
        New(Vec<String>),
        Same(u32),
    }

    pub fn serialize<S: Serializer>(v: &Arc<Vec<String>>, s: S) -> Result<S::Ok, S::Error> {
        let key = Arc::as_ptr(v) as usize;
        SEEN.with(|seen| {
            let mut seen = seen.borrow_mut();
            match seen.get(&key) {
                Some(&i) => Out::Same(i).serialize(s),
                None => {
                    let n = seen.len() as u32;
                    seen.insert(key, n);
                    Out::New(v).serialize(s)
                }
            }
        })
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<Vec<String>>, D::Error> {
        match In::deserialize(d)? {
            In::New(v) => {
                let a = Arc::new(v);
                POOL.with(|p| p.borrow_mut().push(a.clone()));
                Ok(a)
            }
            In::Same(i) => POOL
                .with(|p| p.borrow().get(i as usize).cloned())
                .ok_or_else(|| serde::de::Error::custom("shared lines out of order")),
        }
    }
}

/// serde adapter for `Graph::params` (JSON values: not self-describing here).
pub mod json_text {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &indexmap::IndexMap<String, serde_json::Value>, s: S) -> Result<S::Ok, S::Error> {
        serde_json::to_string(v).map_err(serde::ser::Error::custom)?.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<indexmap::IndexMap<String, serde_json::Value>, D::Error> {
        let t = String::deserialize(d)?;
        serde_json::from_str(&t).map_err(serde::de::Error::custom)
    }
}

// --- content hashes ---------------------------------------------------------------

type Stamp = (i128, u64);

/// Content hash per file of one checkout; a file whose mtime + size match the
/// last run isn't read again (fingerprint.Hashes).
struct Hashes {
    path: PathBuf,
    old: HashMap<String, (Stamp, [u8; 16])>,
    new: HashMap<String, (Stamp, [u8; 16])>,
}

fn stamp_of(path: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    Some((m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128, m.size()))
}

fn blake(data: &[u8]) -> [u8; 16] {
    use blake2::digest::consts::U16;
    use blake2::{Blake2b, Digest};
    let mut h = Blake2b::<U16>::new();
    h.update(data);
    h.finalize().into()
}

impl Hashes {
    fn open(dir: &Path, root: &Path) -> Hashes {
        let name = format!("{}-stamps-rs.bin", &sha1_hex(format!("stamps|{}", root.display()).as_bytes())[..16]);
        let path = dir.join(name);
        let old = std::fs::read(&path).ok().and_then(|b| bincode::deserialize(&b).ok()).unwrap_or_default();
        Hashes { path, old, new: HashMap::new() }
    }

    /// Hashes of `rels` (None for a file that can't be read), read in parallel.
    fn of_all(&mut self, root: &Path, rels: &[&String]) -> Vec<Option<[u8; 16]>> {
        let got: Vec<Option<(Stamp, [u8; 16])>> = rels
            .par_iter()
            .map(|rel| {
                let full = root.join(rel.as_str());
                let st = stamp_of(&full)?;
                if let Some((s, h)) = self.old.get(rel.as_str()) {
                    if *s == st {
                        return Some((st, *h));
                    }
                }
                let data = std::fs::read(&full).ok()?;
                Some((st, blake(&data)))
            })
            .collect();
        for (rel, g) in rels.iter().zip(&got) {
            if let Some(g) = g {
                self.new.insert((*rel).clone(), *g);
            }
        }
        got.into_iter().map(|g| g.map(|x| x.1)).collect()
    }

    fn save(&self) {
        let mut merged = self.old.clone();
        merged.extend(self.new.iter().map(|(k, v)| (k.clone(), *v)));
        if merged != self.old {
            if let Ok(b) = bincode::serialize(&merged) {
                write_atomic(&self.path, &b);
            }
        }
    }
}

fn sha1_hex(b: &[u8]) -> String {
    Sha1::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

/// Atomic: two runs saving at once leave one complete file, never half of each.
fn write_atomic(path: &Path, data: &[u8]) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&tmp, data).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// The files besides the parsed languages' that the graph reads: a manifest
/// or project file changes how imports resolve (imports.rs, held.rs).
fn manifest_like(rel: &str) -> bool {
    matches!(ext_of(rel), ".json" | ".toml" | ".godot" | ".mod" | ".work" | ".gradle" | ".uid")
        && !rel.ends_with(".min.js")
        || rel.ends_with("CMakeLists.txt")
}

/// This binary: a rebuilt seamcheck never reads a graph an older one made.
fn code_version() -> String {
    let stamp = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| format!("{}:{:?}", m.len(), m.modified().ok()))
        .unwrap_or_default();
    format!("{}-{}", crate::VERSION, &sha1_hex(stamp.as_bytes())[..12])
}

/// A cache lookup in progress: the key, and where a graph made now goes.
pub struct Slot {
    dir: PathBuf,
    repo: String,
    name: String,
    hashes: Hashes,
}

/// The cache slot for this project, when the cache is on and the graph is
/// one it keeps (the precise graph depends on language servers: no).
pub fn slot(p: &Project, bridges: bool) -> Option<Slot> {
    Some(slot_in(p, bridges, DIR.get()?.clone()))
}

/// The slot for this project in `dir`, whether or not the process has the
/// cache on (tests keep their own directory).
pub fn slot_in(p: &Project, bridges: bool, dir: PathBuf) -> Slot {
    let t0 = std::time::Instant::now();
    let repo = crate::tidy::exactness::repo_key(&p.root);
    let mut hashes = Hashes::open(&dir, &p.root);
    let rels: Vec<&String> = p.files.iter().filter(|r| crate::lang::ported(r) || manifest_like(r)).collect();
    let digests = hashes.of_all(&p.root, &rels);
    let mut h = Sha1::new();
    h.update(code_version().as_bytes());
    h.update([bridges as u8]);
    h.update(serde_json::to_string(&sorted(&serde_json::Value::Object(p.config.v.clone()))).unwrap_or_default().as_bytes());
    for rel in &p.files {
        h.update(rel.as_bytes());
        h.update([0]);
    }
    for (rel, d) in rels.iter().zip(&digests) {
        h.update(rel.as_bytes());
        h.update([1]);
        match d {
            Some(d) => h.update(d),
            None => h.update([0xff]),
        }
    }
    let key: String = h.finalize().iter().map(|x| format!("{x:02x}")).collect();
    if std::env::var_os("SEAM_TIMING").is_some() {
        eprintln!("  cache key {} ms: {}", t0.elapsed().as_millis(), &key[..24]);
    }
    Slot { name: format!("{repo}-graph-rs-{}.bin", &key[..24]), repo, dir, hashes }
}

fn sorted(v: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
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

impl Slot {
    /// The cached graph, if this key has one.
    pub fn load(&self) -> Option<Graph> {
        let t0 = std::time::Instant::now();
        let bytes = std::fs::read(self.dir.join(&self.name)).ok()?;
        let body = bytes.strip_prefix(MAGIC.as_slice())?;
        POOL.with(|p| p.borrow_mut().clear());
        let g: Graph = match bincode::deserialize(body) {
            Ok(g) => g,
            Err(e) => {
                if std::env::var_os("SEAM_TIMING").is_some() {
                    eprintln!("  cache unreadable: {e}");
                }
                return None;
            }
        };
        POOL.with(|p| p.borrow_mut().clear());
        if std::env::var_os("SEAM_TIMING").is_some() {
            eprintln!("  cache load {} MB: {} ms", bytes.len() / 1_000_000, t0.elapsed().as_millis());
        }
        self.hashes.save();
        self.touch();
        Some(g)
    }

    /// Keep a graph made now, and drop this repository's older ones.
    pub fn save(&self, g: &Graph) {
        SEEN.with(|s| s.borrow_mut().clear());
        // (`serialize` would size the graph first, a pass the shared-lines table can't tell from the real one)
        let mut out = Vec::with_capacity(256 << 20);
        out.extend_from_slice(MAGIC);
        let done = bincode::serialize_into(&mut out, g);
        SEEN.with(|s| s.borrow_mut().clear());
        if done.is_err() {
            return;
        }
        write_atomic(&self.dir.join(&self.name), &out);
        self.hashes.save();
        self.touch();
    }

    /// Put this graph first in the repository's index and delete the ones past `KEEP`.
    fn touch(&self) {
        let index = self.dir.join(format!("{}-graphs-rs.json", self.repo));
        let mut names: Vec<String> =
            std::fs::read_to_string(&index).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        names.retain(|n| *n != self.name);
        names.insert(0, self.name.clone());
        for gone in names.split_off(names.len().min(KEEP)) {
            let _ = std::fs::remove_file(self.dir.join(gone));
        }
        write_atomic(&index, serde_json::to_string(&names).unwrap_or_default().as_bytes());
    }
}
