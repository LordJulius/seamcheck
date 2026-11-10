//! `kind = "design"`: tokens a repo declares, checked in code (src/rules/design.rs).

mod common;

use common::{commit_all, git, load, run, write};
use seamcheck::gate::gate;

const TOKENS: &str = r##"{"spacing":[4,8,12,16],"type":{"body":{"size":16},"small":{"size":12}},
 "themes":{"a":{"accent":"#3fe0c5","void":"#04101a"},"b":{"accent":"#ffb15a"}}}"##;

const DESIGN: &str = r##"
[design]
palette_from = ["design/tokens.json"]
allow_literals = ["#ffffff", "Color.Transparent"]
[design.palette]
paper = "#f4f1ea"
[design.scales.spacing]
props = ["padding", "margin", "gap", "spacing", "spacedBy"]
values = "json:design/tokens.json#spacing"
units = ["px", "dp"]
[design.scales.type]
props = ["fontSize", "font-size"]
values = "json:design/tokens.json#type.*.size"
units = ["px", "sp", "rem"]
[design.scales.radius]
props = ["radius", "rounded", "corner"]
values = []
units = ["px", "dp"]
deny = 'CircleShape|rounded-full'
[design.fonts]
allow = ["Chakra Petch", "Plex Sans"]
deny = ["serif"]
[[rule]]
id = "colour"
kind = "design"
check = "colour"
[[rule]]
id = "space"
kind = "design"
check = "scale"
scale = "spacing"
[[rule]]
id = "type"
kind = "design"
check = "scale"
scale = "type"
[[rule]]
id = "radius"
kind = "design"
check = "scale"
scale = "radius"
[[rule]]
id = "font"
kind = "design"
check = "font"
"##;

fn go(file: &str, body: &str) -> common::Got {
    run(&[("design/tokens.json", TOKENS), (file, body)], &DESIGN.replace("kind = \"design\"", "kind = \"design\"\nin = [\"ui/**\"]"))
}

fn lines(got: &common::Got, rule: &str) -> Vec<usize> {
    got.get(rule).map(|v| v.iter().map(|x| x.1).collect()).unwrap_or_default()
}

#[test]
fn colours_in_the_palette_pass_and_others_are_named() {
    let got = go(
        "ui/a.css",
        ".a { color: #3FE0C5; background: #f4f1ea; border: 1px solid #ffffff }\n.b { color: #3fe0c6 }\n.c { color: rgb(255, 0, 0) }\n.d { color: var(--accent); fill: #abc }\n.e { color: rgb(63, 224, 197) }\n",
    );
    assert!(!got.contains_key("config"), "{:?}", got.get("config"));
    assert_eq!(lines(&got, "colour"), vec![2, 3, 4]);
    assert!(got["colour"][0].2.contains("nearest: accent #3fe0c5"), "{:?}", got["colour"]);
}

#[test]
fn kotlin_and_elixir_colour_ints_and_names() {
    let kt = "val a = Color(0xFF3FE0C5)\nval b = Color(0xFF123456)\nval c = Color.Red\nval d = Color.Transparent\nval flags = 0xFF123456\n";
    assert_eq!(lines(&go("ui/A.kt", kt), "colour"), vec![2, 3]);
    let ex = "t = %{color: 0xFF04101A, bg: \"#112233\"}\n";
    assert_eq!(lines(&go("ui/a.ex", ex), "colour"), vec![1]);
}

#[test]
fn comments_are_not_judged_and_a_waiver_works() {
    let got = go("ui/a.css", "/* #123456 */\n.a { color: #123456 } /* x */\n/* seamcheck: allow colour -- brand */\n.b { color: #654321 }\n");
    assert_eq!(lines(&got, "colour"), vec![2]);
}

#[test]
fn spacing_in_every_syntax() {
    let css = ".a { padding: 8px 13px; margin: 0 auto; gap: 16px; row-gap: 5px; letter-spacing: 0.05em; padding: var(--x) }\n";
    let got = go("ui/a.css", css);
    assert_eq!(got["space"].len(), 2, "{:?}", got["space"]);
    let kt = "Modifier.padding(16.dp).padding(horizontal = 10.dp, vertical = 8.dp).size(48.dp)\nArrangement.spacedBy(6.dp)\nletterSpacing = 1.2.sp\n";
    let got = go("ui/A.kt", kt);
    assert_eq!(lines(&got, "space"), vec![1, 2]);
    let ex = "row(gap: 8, padding_top: 6, padding: 12)\n";
    assert_eq!(go("ui/a.ex", ex)["space"].len(), 1);
    let qml = "Column { spacing: 6; anchors.margins: 16; leftPadding: 0 }\n";
    assert_eq!(go("ui/a.qml", qml)["space"].len(), 1);
    let tsx = "const s = { padding: 13, gap: 8 }\n<div style={{ margin: 4 }} />\n";
    assert_eq!(lines(&go("ui/a.tsx", tsx), "space"), vec![1]);
}

#[test]
fn type_sizes_and_rem() {
    let css = ".a { font-size: 12px } .b { font-size: 0.75rem } .c { font-size: 13px } .d { font-size: 0.72rem }\n";
    assert_eq!(go("ui/a.css", css)["type"].len(), 2);
    let kt = "Text(fontSize = 16.sp)\nText(fontSize = 15.sp)\n";
    assert_eq!(lines(&go("ui/A.kt", kt), "type"), vec![2]);
}

#[test]
fn roundness_is_denied_where_the_scale_has_only_zero() {
    let kt = "shape = RoundedCornerShape(8.dp)\nshape = RoundedCornerShape(0.dp)\nshape = CircleShape\n";
    assert_eq!(lines(&go("ui/A.kt", kt), "radius"), vec![1, 3]);
    assert_eq!(lines(&go("ui/a.css", ".a { border-radius: 4px } .b { border-radius: 0 } .c { @apply rounded-full }\n"), "radius"), vec![1, 1]);
}

#[test]
fn fonts_outside_the_family_and_serifs() {
    let css = ".a { font-family: \"Chakra Petch\", sans-serif }\n.b { font-family: Georgia, serif }\n.c { font-family: var(--font-ui) }\n.d { font-family: \"IBM Plex Sans\", \"JetBrains Mono\", monospace }\n";
    let got = go("ui/a.css", css);
    assert_eq!(lines(&got, "font"), vec![2, 2, 4]);
    let kt = "val f = FontFamily(Font(R.font.chakra_petch_medium))\nval g = FontFamily.Serif\nval h = FontFamily(Font(R.font.plex_sans_regular))\n";
    assert_eq!(lines(&go("ui/A.kt", kt), "font"), vec![2]);
}

#[test]
fn generated_files_are_skipped() {
    let got = go("ui/G.kt", "// Generated from tokens. Do not edit.\nval a = Color(0xFF123456)\n");
    assert!(!got.contains_key("colour"));
}

#[test]
fn bad_config_is_a_finding_not_a_panic() {
    let got = run(&[("a.css", ".a{}\n")], "[[rule]]\nid = \"x\"\nkind = \"design\"\ncheck = \"scale\"\nscale = \"nope\"\n");
    assert!(got["x"][0].2.contains("nope"), "{:?}", got);
}

#[test]
fn the_gate_judges_only_the_changed_lines() {
    common::scratch_cache();
    let d = tempfile::tempdir().unwrap();
    let cfg = format!("[project]\nname = \"t\"\ninclude = [\"ui/**\"]\n{DESIGN}");
    let cfg = cfg.replace("kind = \"design\"", "kind = \"design\"\ngate = \"changed\"\nin = [\"ui/**\"]");
    write(d.path(), &[("design/tokens.json", TOKENS), ("ui/a.css", ".old { color: #123456; padding: 13px }\n"), ("seamcheck.toml", &cfg)]);
    git(d.path(), &["init", "-q"]);
    commit_all(d.path(), "base");
    let mut p = load(d.path());
    assert!(!gate(&mut p, "HEAD", false).failed());
    write(d.path(), &[("ui/a.css", ".old { color: #123456; padding: 13px }\n.new { color: #3fe0c5; padding: 12px }\n")]);
    let mut p = load(d.path());
    assert!(!gate(&mut p, "HEAD", false).failed());
    write(d.path(), &[("ui/a.css", ".old { color: #123456; padding: 13px }\n.new { color: #fedcba; padding: 11px }\n")]);
    let mut p = load(d.path());
    let rep = gate(&mut p, "HEAD", false);
    let mut got: Vec<_> = rep.rules.iter().map(|f| (f.rule.clone(), f.line)).collect();
    got.sort();
    assert_eq!(got, vec![("colour".into(), 2), ("space".into(), 2)]);
}

#[test]
fn words_that_rule_a_property_out() {
    let d = DESIGN.replace("units = [\"px\", \"dp\"]\n[design.scales.type]", "units = [\"px\", \"dp\"]\nnot = [\"letter\"]\n[design.scales.type]");
    let got = run(
        &[("design/tokens.json", TOKENS), ("ui/a.slint", "Text { letter-spacing: 1.2px; spacing: 5px; }\n")],
        &d.replace("kind = \"design\"", "kind = \"design\"\nin = [\"ui/**\"]"),
    );
    assert_eq!(got["space"].len(), 1, "{:?}", got["space"]);
}

#[test]
fn a_font_reference_is_not_a_family_name() {
    let got = go("ui/a.slint", "Text { font-family: Face.display; }\nText { font-family: \"Georgia\"; }\n");
    assert_eq!(lines(&got, "font"), vec![2]);
}
