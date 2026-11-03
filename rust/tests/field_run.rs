//! Bugs a field run found (a Flask repo with its own seamcheck.toml,
//! 2026-11-03), ported from test_field_run.py: a reach rule crashed `check`,
//! one broken rule took every other rule down, a bare `import time` resolved
//! to an unrelated app/utils/time.py, and `.pyw` files were not read at all.

mod common;

use common::write;
use seamcheck::config::Rule;
use seamcheck::project::{Finding, Project};
use seamcheck::rules::{isolate, run_rules};

fn project(files: &[(&str, &str)], rules: &str) -> (tempfile::TempDir, Project) {
    std::env::set_var("SEAMCHECK_RS_QUIET", "1");
    let d = tempfile::tempdir().unwrap();
    write(d.path(), files);
    std::fs::write(d.path().join("seamcheck.toml"), format!("[project]\nname = \"t\"\n{rules}")).unwrap();
    let p = Project::load(d.path(), &[]);
    (d, p)
}

const ROUTES: &[(&str, &str)] = &[
    (
        "app/routes/x.py",
        "from app.auth import _caller, _admin_caller\nfrom app.db import db\n\n\ndef api_list():\n    _caller()\n    return []\n\n\n\
         def api_save():\n    db.session.commit()\n    return 1\n\n\ndef tool_archive():\n    _admin_caller()\n    db.session.commit()\n",
    ),
    ("app/auth.py", "def _caller():\n    return 1\n\n\ndef _admin_caller():\n    return 2\n"),
    ("app/db.py", "class _S:\n    def commit(self):\n        pass\n\n\nclass _Db:\n    session = _S()\n\n\ndb = _Db()\n"),
];

const REACH: &str = r#"
[[rule]]
id = "routes-check-caller"
kind = "reach"
from = ["app/routes/x.py:api_*", "app/routes/x.py:tool_archive"]
to = ["_caller", "_admin_caller"]
depth = 2

[[rule]]
id = "writes-check-caller"
kind = "reach"
from = ["app/routes/x.py:*"]
when = 'db\.session\.commit\(\)'
to = ["_caller", "_admin_caller"]
depth = 2
"#;

// test_field_run.py: ReachRules.test_a_reach_rule_runs_with_and_without_when
#[test]
fn a_reach_rule_runs_with_and_without_when() {
    let (_d, p) = project(ROUTES, REACH);
    let mut got: Vec<(String, String)> = run_rules(&p, None, None).into_iter().map(|f| (f.rule, f.func)).collect();
    got.sort();
    // api_save reaches neither; the rest reach one (any mode)
    assert_eq!(
        got,
        vec![("routes-check-caller".into(), "api_save".into()), ("writes-check-caller".into(), "api_save".into())]
    );
}

// test_field_run.py: BrokenRule.test_a_rule_that_crashes_is_one_finding_and_the_others_run
#[test]
fn a_rule_that_crashes_is_one_finding_and_the_others_run() {
    let (_d, p) = project(&[("a.py", "x = 1  # TODO\n")], "\n[[rule]]\nid = \"todo\"\npattern = \"TODO\"\ntext = \"raw\"\n");
    let rule: &Rule = p.config.rules().next().unwrap();
    std::panic::set_hook(Box::new(|_| {}));
    let got: Vec<Finding> = isolate(rule, || panic!("boom"));
    let _ = std::panic::take_hook();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].severity, "error");
    assert!(got[0].message.contains("rule todo crashed"), "{}", got[0].message);
    // the rule itself, run normally, still reports
    assert!(run_rules(&p, None, None).iter().any(|f| f.rule == "todo"));
}

const IMPORTS: &[(&str, &str)] = &[
    ("app/utils/time.py", "def sleep(n):\n    return n\n"),
    ("app/utils/helpers.py", "def fmt(x):\n    return x\n"),
    ("CKSuper/tools/x/engines/foo.py", "import time\nimport helpers\n\n\ndef go():\n    time.sleep(1)\n    helpers.fmt(1)\n"),
    ("CKSuper/tools/x/engines/near.py", "import local\n\n\ndef go():\n    local.run()\n"),
    ("CKSuper/tools/x/engines/local.py", "def run():\n    return 1\n"),
    ("src/pkg/__init__.py", "def entry():\n    return 1\n"),
    ("tests/test_pkg.py", "import pkg\n\n\ndef test_entry():\n    pkg.entry()\n"),
];

fn callees(g: &seamcheck::graph::Graph, fid: &str, exact_only: bool) -> Vec<String> {
    g.edges.get(fid).map(|v| v.iter().filter(|e| e.2 || !exact_only).map(|e| e.0.clone()).collect()).unwrap_or_default()
}

// test_field_run.py: BareImports (all three tests)
#[test]
fn bare_imports_stdlib_first_and_never_an_unrelated_lone_file() {
    let (_d, p) = project(IMPORTS, "");
    let g = p.graph();
    // a stdlib name is the stdlib's
    assert!(g.modules["CKSuper/tools/x/engines/foo"].external.contains("time"));
    let foo = callees(&g, "CKSuper/tools/x/engines/foo.go", false);
    assert!(!foo.contains(&"app/utils/time.sleep".to_string()), "{foo:?}");
    // a lone file in an unrelated folder is not imported
    assert!(!foo.contains(&"app/utils/helpers.fmt".to_string()), "{foo:?}");
    // a module beside the importer and a package elsewhere still are
    assert!(callees(&g, "CKSuper/tools/x/engines/near.go", true).contains(&"CKSuper/tools/x/engines/local.run".to_string()));
    assert!(callees(&g, "tests/test_pkg.test_entry", true).contains(&"src/pkg/__init__.entry".to_string()));
}

// test_field_run.py: PywFiles.test_a_pyw_file_is_python
#[test]
fn a_pyw_file_is_python() {
    let (_d, p) = project(
        &[("launcher.pyw", "import sys\n\n\ndef download(url):\n    return fetch(url)\n\n\ndef fetch(u):\n    return u\n")],
        "\n[[rule]]\nid = \"dl\"\nkind = \"require\"\nin = [\"launcher.pyw\"]\nfunc = \"download\"\npattern = \"fetch\"\n",
    );
    assert!(!run_rules(&p, None, None).iter().any(|f| f.rule == "dl"));
    let g = p.graph();
    assert_eq!(g.find("download").iter().map(|f| f.id.clone()).collect::<Vec<_>>(), vec!["launcher.download".to_string()]);
    assert!(callees(&g, "launcher.download", false).contains(&"launcher.fetch".to_string()));
    assert_eq!(seamcheck::project::scan_family("launcher.pyw"), "python");
}
