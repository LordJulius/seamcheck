//! `seamcheck verify`: drive the real app by the ids of its controls, keep
//! what it showed, and judge that evidence with the runtime rules
//! (docs/verify.md).
//!
//!   seamcheck verify --list [TARGET]
//!   seamcheck verify --doctor TARGET
//!   seamcheck verify TARGET [FEATURE...]
//!   seamcheck verify --prove TARGET [FEATURE...]
//!   seamcheck verify --check [--staged | --since REF]

pub mod adb;
pub mod check;
pub mod config;
pub mod driver;
pub mod evidence;
pub mod run;
pub mod tree;

use std::path::{Path, PathBuf};

use config::{load_features, load_target, targets, Feature, Target};

const USAGE: &str = "usage: seamcheck verify [-C DIR] (--list [TARGET] | --doctor TARGET | --check [--staged | --since REF] | [--prove] TARGET [FEATURE...])";

fn usage(msg: &str) -> i32 {
    eprintln!("{USAGE}");
    eprintln!("seamcheck verify: {msg}");
    2
}

/// The nearest directory at or above `start` with `seamcheck.d/verify` (or a seamcheck.toml).
fn find_root(start: &Path) -> PathBuf {
    let start = std::path::absolute(start).unwrap_or_else(|_| start.to_path_buf());
    let mut d = start.clone();
    loop {
        if d.join(config::DIR).is_dir() || d.join("seamcheck.toml").exists() {
            return d;
        }
        match d.parent() {
            Some(p) => d = p.to_path_buf(),
            None => return start,
        }
    }
}

pub fn main(argv: &[String]) -> i32 {
    let mut root = PathBuf::from(".");
    let (mut list, mut doctor, mut prove, mut check, mut staged) = (false, false, false, false, false);
    let mut since: Option<String> = None;
    let mut pos: Vec<String> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "-C" => {
                i += 1;
                match argv.get(i) {
                    Some(d) => root = PathBuf::from(d),
                    None => return usage("-C needs a directory"),
                }
            }
            "--list" => list = true,
            "--doctor" => doctor = true,
            "--prove" => prove = true,
            "--check" => check = true,
            "--staged" => staged = true,
            "--since" => {
                i += 1;
                match argv.get(i) {
                    Some(r) => since = Some(r.clone()),
                    None => return usage("--since needs a ref"),
                }
            }
            "-h" | "--help" => {
                println!("{USAGE}\n\nSee docs/verify.md in the seamcheck repo.");
                return 0;
            }
            x if x.starts_with('-') => return usage(&format!("unrecognized argument {x}")),
            x => pos.push(x.to_string()),
        }
        i += 1;
    }
    if [list, doctor, prove, check].iter().filter(|x| **x).count() > 1 {
        return usage("--list, --doctor, --prove and --check are separate runs");
    }
    let root = find_root(&root);
    if check {
        return check::main(&root, staged, since.as_deref());
    }
    if list {
        return list_targets(&root, pos.first().map(String::as_str));
    }
    let Some(name) = pos.first() else { return usage("which target? (seamcheck verify --list)") };
    let (t, problems) = match load_target(&root, name) {
        Ok(x) => x,
        Err(e) => {
            println!("[error] {e}");
            return 2;
        }
    };
    let (features, fproblems) = load_features(&t);
    if !problems.is_empty() || !fproblems.is_empty() {
        for p in problems.iter().chain(&fproblems) {
            println!("[error] config: {p}");
        }
        return 2;
    }
    if doctor {
        return run::doctor_only(&root, &t);
    }
    let picked: Vec<&Feature> = if pos.len() > 1 {
        let mut out = Vec::new();
        for want in &pos[1..] {
            match features.iter().find(|f| &f.id == want) {
                Some(f) => out.push(f),
                None => {
                    let ids: Vec<&str> = features.iter().map(|f| f.id.as_str()).collect();
                    println!("[error] no feature '{want}' in {} (has: {})", t.name, ids.join(", "));
                    return 2;
                }
            }
        }
        out
    } else {
        features.iter().collect()
    };
    if picked.is_empty() {
        println!("[error] {} has no features ({}/)", t.name, t.feature_dir().display());
        return 2;
    }
    run::features(&root, &t, &picked, prove)
}

fn list_targets(root: &Path, only: Option<&str>) -> i32 {
    let names = targets(root);
    if names.is_empty() {
        println!("no verify targets under {}", root.join(config::DIR).display());
        return 0;
    }
    for name in names.iter().filter(|n| only.is_none_or(|o| o == n.as_str())) {
        let (t, problems): (Target, Vec<String>) = match load_target(root, name) {
            Ok(x) => x,
            Err(e) => {
                println!("{name}: [error] {e}");
                continue;
            }
        };
        let (features, fproblems) = load_features(&t);
        let proof = check::read_proof(&t);
        println!("{name} ({} driver){}", t.driver(), if t.str("target", "why").is_empty() { String::new() } else { format!(": {}", t.str("target", "why")) });
        for p in problems.iter().chain(&fproblems) {
            println!("  [error] {p}");
        }
        for f in &features {
            let state = check::proof_state(&t, f, &proof);
            println!("  {:<28} {:<8} {}", f.id, state, f.title);
        }
    }
    0
}
