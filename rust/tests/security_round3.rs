//! The security pack's round-3 summaries on a loaded graph, ported from
//! tests/test_security_round3.py (RoundThreeHelpers.test_summaries_by_name).

mod common;

use std::collections::HashSet;

use common::write;
use seamcheck::graph::Graph;
use seamcheck::model::Func;
use seamcheck::project::Project;
use seamcheck::rules::security::summaries;

const H: &str = "from pathlib import Path\nfrom flask import url_for\n\n\n\
    def check(p):\n    r = Path(p).resolve()\n    if not r.is_relative_to('/srv'):\n        raise ValueError(p)\n    return r\n\n\n\
    def managed(p):\n    return check(p)\n\n\n\
    def loose(p):\n    return Path(p).resolve()\n\n\n\
    def mq(e, q):\n    return url_for(e) + '?' + q\n\n\n\
    def go(q):\n    return mq('x', q)\n\n\n\
    def off(q):\n    return q + '/x'\n";

#[test]
fn summaries_by_name() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), &[("app/h.py", H)]);
    let p = Project::load(d.path(), &[]);
    let g = Graph::load(&p);
    // (a function's lines; the path rule passes its own code, nested functions blanked)
    let code = |f: &Func| g.modules.get(&f.module).map(|m| m.raw[f.start - 1..f.end.min(m.raw.len())].join("\n")).unwrap_or_default();
    let (confined, same, _free) = summaries(&g, &code);
    let set = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<HashSet<String>>();
    // (names and function ids)
    assert_eq!(set(&["check", "managed", "app/h.check", "app/h.managed"]), confined);
    assert_eq!(set(&["mq", "go", "app/h.mq", "app/h.go"]), same);
}
