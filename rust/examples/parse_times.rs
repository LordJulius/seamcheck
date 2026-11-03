//! The slowest files to parse, and the graph's phases:
//! `cargo run --release --example parse_times -- REPO`
use std::time::Instant;

fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("REPO"));
    let p = seamcheck::project::Project::load(&root, &[]);
    let res = seamcheck::graph::res_map(&p.files);
    let mut times: Vec<(u128, String)> = Vec::new();
    let t = Instant::now();
    for rel in p.files.iter().filter(|r| seamcheck::lang::ported(r)) {
        let t1 = Instant::now();
        let _ = seamcheck::graph::parse_file(&p, &res, rel);
        times.push((t1.elapsed().as_millis(), rel.clone()));
    }
    println!("serial parse: {} ms", t.elapsed().as_millis());
    times.sort();
    for (ms, rel) in times.iter().rev().take(25) {
        println!("{ms:>7} ms  {rel}");
    }
    let mut by_ext: std::collections::BTreeMap<&str, u128> = Default::default();
    for (ms, rel) in &times {
        *by_ext.entry(seamcheck::project::ext_of(rel)).or_default() += ms;
    }
    println!("{by_ext:?}");
    let t = Instant::now();
    let g = seamcheck::graph::Graph::load_with(&p, false);
    println!("graph (parallel parse + link): {} ms, {} funcs", t.elapsed().as_millis(), g.funcs.len());
}
