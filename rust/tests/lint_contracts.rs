//! tests/test_contracts_ui.py, the lint parts: `kind = "essential"` (controls
//! a bar always shows) and a lint rule's `gate = "changed"`.

mod common;
use common::run;

use seamcheck::project::Project;
use seamcheck::rules::run_rules;

const BAR_BEFORE: &str = r#"
    export function CommandInput({ onSend, onCancel }) {
      return (
        <div className="command-dock">
          <div className="command-row">
            <div className="command-menu-anchor">
              <button type="button" data-testid="parity-model-chip" onClick={() => setChipMenu((o) => !o)}>Model</button>
              {chipMenu ? (
                <div className="v-menu command-pop" aria-label="Veridian">
                  <button type="button" aria-label="Minimize chat dock" onClick={() => setDockMinimized(true)}>
                    Minimize chat
                  </button>
                  {!busy || autoApproveArmed ? (
                    <button type="button" data-testid="parity-auto-approve" disabled={busy} onClick={toggle}>
                      Auto-approve
                    </button>
                  ) : null}
                </div>
              ) : null}
            </div>
            <button type="button" className="command-send" data-testid="parity-send" onClick={submit}
              disabled={!text.trim() || uploading}>Send</button>
            {busy && (
              <button type="button" className="command-cancel" data-testid="parity-abort" onClick={onCancel}>Abort</button>
            )}
          </div>
        </div>
      );
    }
"#;
const BAR_AFTER: &str = r#"
    export function CommandInput({ onSend, onCancel }) {
      return (
        <div className="command-dock">
          <div className="command-row">
            <div className="command-menu-anchor">
              <button type="button" data-testid="parity-model-chip" onClick={() => setChipMenu((o) => !o)}>Model</button>
              {chipMenu ? <div className="v-menu command-pop">...</div> : null}
            </div>
            <button type="button" data-testid="parity-auto-approve" aria-pressed={on} onClick={toggle}>Auto-approve</button>
            <button type="button" aria-label="Minimize chat dock" onClick={() => setDockMinimized(true)}>Minimize</button>
            <button type="button" className="command-send" data-testid="parity-send" onClick={submit}
              disabled={!text.trim() || uploading}>Send</button>
            {busy && (
              <button type="button" className="command-cancel" data-testid="parity-abort" onClick={onCancel}>Abort</button>
            )}
          </div>
        </div>
      );
    }
"#;
const BAR_RULE: &str = r#"
    [[rule]]
    id = "chat-bar"
    kind = "essential"
    in = ["CommandInput.tsx"]
    bar = 'className="command-row"'
    [rule.controls]
    send = 'data-testid="parity-send"'
    auto_approve = { marker = 'data-testid="parity-auto-approve"', disabled = false }
    minimize = 'aria-label="Minimize chat dock"'
    abort = { marker = 'data-testid="parity-abort"', when = '^busy\b' }
"#;

const COMPOSER_KT: &str = r#"
    @Composable
    fun VoiceChatComposer(vm: Vm, chat: Chat) {
        var menuOpen by remember { mutableStateOf(false) }
        Column(M.fillMaxWidth()) {
            Row(M.fillMaxWidth()) {
                Box(M.background(if (sendEnabled) A else B).clickable(enabled = sendEnabled) { send() }.testTag("chat_send")) {}
                if (chat.primaryBusy) {
                    Box(M.clickable { vm.abort() }.testTag("chat_abort")) {}
                }
                Box {
                    DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                        DropdownMenuItem(text = { Text("AUTO") }, onClick = { vm.toggle() }, modifier = M.testTag("chat_auto_approve"))
                    }
                }
            }
        }
    }
"#;
const COMPOSER_RULE: &str = r#"
    [[rule]]
    id = "osa-bar"
    kind = "essential"
    in = ["VoiceChatBar.kt"]
    bar = 'fun VoiceChatComposer\('
    controls = { send = 'testTag\("chat_send"\)', abort = 'testTag\("chat_abort"\)', auto_approve = 'testTag\("chat_auto_approve"\)', gone = 'testTag\("chat_gone"\)' }
"#;

const CHAT_QML: &str = r#"
    Rectangle {
      Row {
        id: composerRow
        DeskButton { text: "Send"; onClicked: chat.sendDraft() }
        DeskButton {
          text: "auto"
          visible: !chat.busy
          onClicked: continuity.chatAutoApprove = !continuity.chatAutoApprove
        }
      }
    }
"#;
const QML_RULE: &str = r#"
    [[rule]]
    id = "osd-bar"
    kind = "essential"
    in = ["ChatHost.qml"]
    bar = 'id:\s*composerRow'
    controls = { send = 'text:\s*"Send"', auto_approve = 'chatAutoApprove = !continuity' }
"#;

#[test]
fn minimize_and_auto_approve_were_hidden_in_the_chip_menu() {
    let got = &run(&[("CommandInput.tsx", BAR_BEFORE)], BAR_RULE)["chat-bar"];
    let mut msgs: Vec<&str> = got.iter().map(|g| g.2.as_str()).collect();
    msgs.sort();
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    assert!(msgs[0].contains("'auto_approve' can be disabled (disabled={busy})"), "{}", msgs[0]);
    assert!(msgs[1].contains("'minimize' is inside a menu"), "{}", msgs[1]);
    assert!(!run(&[("CommandInput.tsx", BAR_AFTER)], BAR_RULE).contains_key("chat-bar"));
}

#[test]
fn abort_without_its_allowed_condition() {
    let rule = BAR_RULE.replace(
        "abort = { marker = 'data-testid=\"parity-abort\"', when = '^busy\\b' }",
        "abort = 'data-testid=\"parity-abort\"'",
    );
    let got = &run(&[("CommandInput.tsx", BAR_AFTER)], &rule)["chat-bar"];
    assert_eq!(got.len(), 1);
    assert!(got[0].2.contains("'abort' renders only when {busy &&"), "{}", got[0].2);
}

#[test]
fn compose_bar() {
    let got = &run(&[("VoiceChatBar.kt", COMPOSER_KT)], COMPOSER_RULE)["osa-bar"];
    let by: std::collections::BTreeMap<String, String> =
        got.iter().map(|g| (g.2.split('\'').nth(1).unwrap().to_string(), g.2.clone())).collect();
    assert_eq!(by.keys().cloned().collect::<Vec<_>>(), vec!["abort", "auto_approve", "gone"]);
    assert!(by["abort"].contains("renders only when if (chat.primaryBusy)"), "{}", by["abort"]);
    assert!(by["auto_approve"].contains("inside a menu (DropdownMenu"), "{}", by["auto_approve"]);
    assert!(by["gone"].contains("is gone"));
}

#[test]
fn qml_bar() {
    let got = &run(&[("ChatHost.qml", CHAT_QML)], QML_RULE)["osd-bar"];
    assert_eq!(got.len(), 1);
    assert!(got[0].2.contains("'auto_approve' renders only when visible: !chat.busy"), "{}", got[0].2);
}

fn git(root: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

#[test]
fn old_debt_shows_new_debt_blocks() {
    std::env::set_var("SEAMCHECK_RS_QUIET", "1");
    let cfg = "[project]\nname = \"t\"\ninclude = [\"**\"]\n[[rule]]\nid = \"ids\"\nkind = \"lint\"\nlint = \"id-kind-mix\"\nseverity = \"warn\"\ngate = \"changed\"\n";
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    common::write(root, &[("seamcheck.toml", cfg), ("A.kt", "fun a(n: N, dest: D) = put(n.requestId ?: dest.id)\n"), ("B.kt", "fun b() = 1\n")]);
    git(root, &["init", "-q"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "base"]);
    std::fs::write(root.join("B.kt"), "fun b() = 2\n").unwrap();
    let p = Project::load(root, &[]);
    assert_eq!(run_rules(&p, None, None).iter().filter(|f| f.rule == "ids").count(), 1);
    let mut p = Project::load(root, &[]);
    assert!(seamcheck::gate::gate(&mut p, "HEAD", false).rules.is_empty());
    std::fs::write(root.join("B.kt"), "fun b(n: N, dest: D) = put(n.challengeId ?: dest.id)\n").unwrap();
    let mut p = Project::load(root, &[]);
    let rep = seamcheck::gate::gate(&mut p, "HEAD", false);
    assert_eq!(rep.rules.iter().map(|f| (f.file.clone(), f.line)).collect::<Vec<_>>(), vec![("B.kt".to_string(), 1)]);
}

#[test]
fn bad_gate_value() {
    let got = run(&[("a.kt", "fun a() = 1\n")], "[[rule]]\nid = \"x\"\nkind = \"lint\"\nlint = \"id-kind-mix\"\ngate = \"sometimes\"\n");
    assert!(got["config"][0].2.contains("gate 'sometimes'"));
}
