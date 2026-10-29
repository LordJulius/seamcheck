//! `seamcheck verify` without a device: where evidence may go, what a tree
//! capture records, the config's refusals, and the offline `--check`.

use std::path::Path;
use std::process::Command;

use seamcheck::verify::config::{load_features, load_target};
use seamcheck::verify::evidence;
use seamcheck::verify::tree;

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git").args(args).current_dir(dir).output().unwrap().status.success();
    assert!(ok, "git {args:?}");
}

const TARGET: &str = r#"
[target]
driver = "adb"
covers = ["lib/*_screen.ex"]
[adb]
device = "SERIAL"
deny = ["SM-F9*"]
package = "org.example.app"
"#;

const FEATURE: &str = r#"
[feature]
id = "home.open"
title = "Open home"
screens = ["lib/home_screen.ex"]
steps = [{ tap = "home.go" }, { tap = "home.row.3", expect = "unchanged" }]

[[contract]]
id = "goes"
act = ["tap:home.go"]
assert = "'home.page' in after.ids"
"#;

fn project(evidence_dir: Option<&str>) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q"]);
    let mut t = TARGET.to_string();
    if let Some(dir) = evidence_dir {
        t.push_str(&format!("[evidence]\ndir = \"{dir}\"\n"));
    }
    write(&d.path().join("seamcheck.d/verify/app.toml"), &t);
    write(&d.path().join("seamcheck.d/verify/app/home.toml"), FEATURE);
    write(&d.path().join("lib/home_screen.ex"), "id: :\"home.go\"\nid: :\"home.row.#{n}\"\n");
    d
}

#[test]
fn evidence_defaults_outside_the_repo() {
    let d = project(None);
    let (t, problems) = load_target(d.path(), "app").unwrap();
    assert!(problems.is_empty(), "{problems:?}");
    let base = evidence::base(d.path(), &t).unwrap();
    assert!(!base.starts_with(d.path()), "{}", base.display());
    assert!(base.ends_with(Path::new("seamcheck/verify").join(d.path().file_name().unwrap()).join("app")));
}

#[test]
fn evidence_inside_the_work_tree_must_be_ignored() {
    let d = project(Some(".evidence"));
    let (t, _) = load_target(d.path(), "app").unwrap();
    let err = evidence::base(d.path(), &t).unwrap_err();
    assert!(err.contains("not ignored by git"), "{err}");
    write(&d.path().join(".gitignore"), "/.evidence/\n");
    let ok = evidence::base(d.path(), &t).unwrap();
    assert!(ok.starts_with(d.path().join(".evidence")) || ok.ends_with(".evidence/app"), "{}", ok.display());
}

#[test]
fn evidence_outside_the_work_tree_is_allowed() {
    let out = tempfile::tempdir().unwrap();
    let d = project(Some(out.path().to_str().unwrap()));
    let (t, _) = load_target(d.path(), "app").unwrap();
    assert_eq!(evidence::base(d.path(), &t).unwrap(), out.path().join("app"));
}

#[test]
fn typos_and_coordinates_are_config_errors() {
    let d = project(None);
    write(
        &d.path().join("seamcheck.d/verify/app/bad.toml"),
        "[feature]\nid = \"bad\"\ncaptur = \"tree\"\nsteps = [{ x = 10, y = 20 }, { tap = \"a.b\", expect = \"nothing\" }]\n",
    );
    let (t, _) = load_target(d.path(), "app").unwrap();
    let (fs, problems) = load_features(&t);
    let text = problems.join("\n");
    assert!(text.contains("unknown key 'captur' (did you mean 'capture'?)"), "{text}");
    assert!(text.contains("step 1: unknown key 'x'"), "{text}");
    assert!(text.contains("expect 'nothing'"), "{text}");
    assert_eq!(fs.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), vec!["bad", "home.open"]);
}

const DUMP: &str = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><hierarchy rotation="0">
<node index="0" text="" resource-id="" content-desc="" package="org.example.app" bounds="[0,0][1080,2000]" enabled="true" selected="false" checked="false">
  <node index="0" text="" resource-id="vault.pin_pad" content-desc="" package="org.example.app" bounds="[0,0][1080,1800]" enabled="true" selected="false" checked="false">
    <node index="0" text="Enter PIN" resource-id="vault.pin.prompt" content-desc="" package="org.example.app" bounds="[100,100][900,200]" enabled="true" selected="false" checked="false" />
    <node index="1" text="" resource-id="vault.pin.1" content-desc="" package="org.example.app" bounds="[100,300][300,400]" enabled="true" selected="false" checked="false">
      <node index="0" text="1" resource-id="" content-desc="" package="org.example.app" bounds="[150,320][250,380]" enabled="true" selected="false" checked="false" />
    </node>
  </node>
  <node index="1" text="secret scene title" resource-id="vault.item.0" content-desc="" package="org.example.app" bounds="[0,1800][1080,1900]" enabled="true" selected="false" checked="false" />
  <node index="2" text="" resource-id="" content-desc="nav.home" package="org.example.app" bounds="[0,1900][540,2000]" enabled="true" selected="true" checked="false" />
  <node index="3" text="" resource-id="" content-desc="Home: 3 new" package="org.example.app" bounds="[540,1900][1080,2000]" enabled="true" selected="false" checked="false" />
</node>
<node index="1" text="12:00" resource-id="com.android.systemui:id/clock" content-desc="" package="com.android.systemui" bounds="[0,0][100,50]" enabled="true" selected="false" checked="false" />
</hierarchy>"#;

fn id_re() -> regex::Regex {
    regex::Regex::new(r"^[a-z][a-z0-9_]*(\.[a-z0-9_<>-]+)+$").unwrap()
}

#[test]
fn ids_come_from_resource_id_or_an_id_shaped_content_desc() {
    let s = tree::parse(DUMP, &id_re()).unwrap();
    assert_eq!(s.ids("org.example.app"), vec!["vault.pin_pad", "vault.pin.prompt", "vault.pin.1", "vault.item.0", "nav.home"]);
    let rec = s.record("org.example.app", &[]);
    assert_eq!(rec["els"]["vault.pin.1"]["text"], "1");
    assert_eq!(rec["els"]["nav.home"]["selected"], true);
    assert!(!rec["texts"].as_array().unwrap().iter().any(|t| t == "12:00"), "another app's nodes are not the app's");
}

#[test]
fn a_tree_capture_records_only_the_kept_subtrees_and_counts_the_rest() {
    let s = tree::parse(DUMP, &id_re()).unwrap();
    let keep = vec!["vault.pin*".to_string()];
    let rec = s.record("org.example.app", &keep);
    let ids: Vec<&str> = rec["ids"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(ids, vec!["vault.pin_pad", "vault.pin.prompt", "vault.pin.1"]);
    assert_eq!(rec["outside"]["ids"], 2);
    assert_eq!(rec["outside"]["texts"], 1);
    // a private area's ids are counted, never named; other ids are names, not content
    assert_eq!(rec["outside"]["names"], serde_json::json!(["nav.home"]));
    let xml = s.filtered_xml("org.example.app", &keep);
    assert!(!xml.contains("secret"), "{xml}");
    assert!(!xml.contains("vault.item"), "{xml}");
}

#[test]
fn touches_land_inside_the_element() {
    assert_eq!(tree::point([0, 0, 100, 200], "").unwrap(), (50, 100));
    assert_eq!(tree::point([0, 0, 100, 200], "top").unwrap(), (50, 12));
    assert!(tree::point([0, 0, 100, 200], "10,20").is_err());
}

fn verify(dir: &Path, args: &[&str]) -> (i32, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_seamcheck")).arg("verify").args(args).current_dir(dir).output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned())
}

#[test]
fn check_finds_renamed_ids_drafts_and_uncovered_screens() {
    let d = project(None);
    write(&d.path().join("seamcheck.d/verify/app/gone.toml"), "[feature]\nid = \"gone\"\nscreens = [\"lib/home_screen.ex\"]\nsteps = [{ tap = \"home.renamed\" }]\n");
    let (code, out) = verify(d.path(), &["--check"]);
    assert_eq!(code, 1);
    assert!(out.contains("gone.toml:4: step id 'home.renamed' is in none"), "{out}");
    assert!(!out.contains("'home.row.3'"), "a stem the code fills in counts: {out}");
    assert!(out.contains("feature 'home.open' is a draft"), "{out}");
    write(&d.path().join("lib/mail_screen.ex"), "x\n");
    git(d.path(), &["add", "lib/mail_screen.ex"]);
    let (_, out) = verify(d.path(), &["--check", "--staged"]);
    assert!(out.contains("lib/mail_screen.ex:1: a app screen no verify feature covers"), "{out}");
    assert!(!out.contains("is a draft"), "with a change in hand, only touched features: {out}");
}

#[test]
fn doctor_refuses_an_unnamed_device() {
    let d = project(None);
    write(&d.path().join("seamcheck.d/verify/app.toml"), &TARGET.replace("SERIAL", "$SEAMCHECK_TEST_NO_SUCH_VAR"));
    let (code, out) = verify(d.path(), &["--doctor", "app"]);
    assert_eq!(code, 2);
    assert!(out.contains("verify never picks a device"), "{out}");
}
