//! Where a run's time goes: `cargo run --release --example phases -- REPO [RULE...]`
//! times loading, reading, blanking each file in each mode, the Elixir
//! parse, and then each named rule alone.

use std::time::Instant;

use seamcheck::project::{Mode, Project};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = std::path::PathBuf::from(args.first().expect("REPO"));
    let t = Instant::now();
    let p = Project::load(&root, &[]);
    println!("load (config + walk): {} files, {} ms", p.files.len(), t.elapsed().as_millis());
    let t = Instant::now();
    let bytes: usize = p.files.iter().map(|f| p.text(f).len()).sum();
    println!("read: {} MB, {} ms", bytes / 1_000_000, t.elapsed().as_millis());
    let texts: Vec<(String, std::sync::Arc<String>)> = p.files.iter().map(|f| (f.clone(), p.text(f))).collect();
    let t = Instant::now();
    let mut nb = 0;
    for (f, x) in &texts {
        nb += seamcheck::scan::blank(x, seamcheck::project::scan_family(f), true).0.len();
    }
    println!("blank alone: {} MB in {} ms", nb / 1_000_000, t.elapsed().as_millis());
    for mode in [Mode::NoComment, Mode::Code] {
        let t = Instant::now();
        for f in &p.files {
            p.text_for(f, mode);
        }
        println!("blank {mode:?}: {} ms", t.elapsed().as_millis());
    }
    let t = Instant::now();
    println!("elixir modules: {} in {} ms", p.module_names().len(), t.elapsed().as_millis());
    for rid in &args[1..] {
        let r = p.config.rules().find(|r| seamcheck::config::id(r) == rid).expect("rule");
        let t = Instant::now();
        let n = seamcheck::rules::run_one(&p, r).len();
        println!("rule {rid}: {n} findings, {} ms", t.elapsed().as_millis());
    }
}
