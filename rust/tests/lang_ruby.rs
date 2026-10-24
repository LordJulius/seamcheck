//! The Ruby adapter, ported from the Python suite (tests/test_ruby.py).
//!
//! The parse-level tests run here. The ones that go through `core.load` and
//! read edges need Ruby's receiver types (`graph/infer.rs`, not ported yet);
//! they are in `graph_cases` below, ignored until that lane lands, with the
//! Python assertions kept as written. `link_state_matches_python` is the
//! proof for what is ported of lang_ruby.link.

mod common;

use std::collections::{BTreeSet, HashMap};

use common::dedent;
use seamcheck::lang::ruby;
use seamcheck::model::Module;
use seamcheck::scan::blank;

/// textwrap.dedent of a literal that starts on the line after its opening quote
fn d(s: &str) -> String {
    dedent(s.strip_prefix('\n').unwrap_or(s))
}

fn parse(text: &str, rel: &str) -> Vec<Module> {
    ruby::parse(rel, &d(text))
}

/// {"<module>.<name>": (start, end)} (test_ruby.spans)
fn spans(mods: &[Module]) -> HashMap<String, (usize, usize)> {
    mods.iter()
        .flat_map(|m| m.funcs.iter())
        .filter(|f| f.kind != "top")
        .map(|f| {
            let short = f.module.split_once('.').map(|x| x.1).unwrap_or(&f.module);
            (format!("{short}.{}", f.name), (f.start, f.end))
        })
        .collect()
}

// test_ruby.py: RubyLiteralsTest.test_a_literal_hides_its_keywords_and_keeps_interpolated_calls
#[test]
fn a_literal_hides_its_keywords_and_keeps_interpolated_calls() {
    // a heredoc / %w() / regex / =begin holding `end` or a quote shifted every block after it;
    // `"#{foo("b")}"` ended the string at the inner quote
    let src = d(r##"
x = "a #{foo("b")} c" # end
y = %w(def end class)
z = <<~EOS.strip
  def nope
  #{bar(1)}
  end
EOS
r = x =~ /end"[/]#/ ; q = a / b
=begin
def hidden
end
=end
w = ?" ; v = $"
"##);
    let (code, strings) = blank(&src, "ruby", false);
    assert_eq!(code.len(), src.len());
    let lines: Vec<&str> = code.split('\n').collect();
    assert!(lines[0].contains(r#"foo(" ")"#));
    let hay = format!("{}{}{}{}{}", lines[0], lines[1], lines[3], lines[5], lines[7]);
    assert!(!hay.contains("end"));
    assert!(lines[4].contains("bar(1)"));
    assert!(lines[7].contains("q = a / b")); // a division, not a regex
    assert!(!code.contains("hidden"));
    assert!(strings.iter().any(|(_, s)| s == "def end class"));
}

// test_ruby.py: RubyLiteralsTest.test_other_languages_blank_as_before
#[test]
fn other_languages_blank_as_before() {
    assert_eq!(blank("x = 'a' # c\n", "python", false).0, "x = ' '    \n");
}

// test_ruby.py: RubyBlocksTest.test_modifiers_operands_and_loops_open_the_right_blocks
#[test]
fn modifiers_operands_and_loops_open_the_right_blocks() {
    // sinatra's `host << if request.forwarded?` closed the module early;
    // `return a || /.*/ unless b` (a regex before `unless`) opened one
    let mods = parse(
        r#"
module Sinatra
  module Helpers
    def uri(addr = nil)
      host << if request.forwarded?
                request.host_with_port
              else
                request.host
              end
      return addr if addr
      while x do
        x -= 1
      end
      host
    end

    def prefixed_path(a, b)
      return a || b || /.*/ unless a && b
      b
    end

    def size = @size

    def block.each; yield(call) end
    def @@eats.flush(*) end

    def last; end
  end
end
"#,
        "lib/a.rb",
    );
    let got = spans(&mods);
    assert_eq!(got["Sinatra::Helpers.uri"], (3, 14));
    assert_eq!(got["Sinatra::Helpers.prefixed_path"], (16, 19));
    assert_eq!(got["Sinatra::Helpers.size"], (21, 21)); // endless: no `end`
    assert_eq!(got["Sinatra::Helpers.last"], (26, 26));
    let names: BTreeSet<&str> = mods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, BTreeSet::from(["lib/a", "lib/a.Sinatra", "lib/a.Sinatra::Helpers"]));
}

// test_ruby.py: RubyBlocksTest.test_rspec_minitest_and_shoulda_blocks_are_tests
#[test]
fn rspec_minitest_and_shoulda_blocks_are_tests() {
    let mods = parse(
        r#"
describe Foo do
  it "does a thing" do
    run_it
  end
end

class FooTest < Minitest::Test
  def test_bar
    bar
  end

  test "baz works" do
    baz
  end

  context "with x" do
    should "qux" do
      qux
    end
  end
end
"#,
        "test/foo_test.rb",
    );
    let tests: BTreeSet<&str> =
        mods.iter().flat_map(|m| m.funcs.iter()).filter(|f| f.kind == "test").map(|f| f.name.as_str()).collect();
    assert_eq!(tests, BTreeSet::from(["test does a thing", "test_bar", "test baz works", "test qux"]));
    let calls: BTreeSet<&str> =
        mods.iter().flat_map(|m| m.funcs.iter()).flat_map(|f| f.calls.iter()).map(|c| c.name.as_str()).collect();
    for want in ["run_it", "bar", "baz", "qux"] {
        assert!(calls.contains(want), "{want} in {calls:?}");
    }
    // `context` / `it` are recorded too: the framework's (no edge), or a project's own
    // DSL when it defines one (sinatra's test/contest.rb; see RubyProjectDslTest)
}

// test_ruby.py: RubyCallsTest.test_calls_need_no_parentheses_and_locals_setters_keys_are_not_calls
#[test]
fn calls_need_no_parentheses_and_locals_setters_keys_are_not_calls() {
    let mods = parse(
        r#"
class Site
  def process(config)
    reader = build_reader
    reader.read config
    self.config = config
    render json: payload, layout: false
    count = 0
    count
  end
end
"#,
        "lib/a.rb",
    );
    let f = mods.iter().flat_map(|m| m.funcs.iter()).find(|f| f.name == "process").unwrap();
    let calls: BTreeSet<(String, Vec<String>)> = f.calls.iter().map(|c| (c.name.clone(), c.chain.clone())).collect();
    assert!(calls.contains(&("build_reader".to_string(), vec![])));
    assert!(calls.contains(&("read".to_string(), vec!["reader".to_string()])));
    assert!(calls.contains(&("render".to_string(), vec![])));
    assert!(calls.contains(&("payload".to_string(), vec![]))); // a bare name nobody assigned: a method call
    let names: BTreeSet<&str> = f.calls.iter().map(|c| c.name.as_str()).collect();
    assert!(!names.contains("config")); // a parameter, and `self.config =` is a setter
    assert!(!names.contains("json")); // a hash key
    assert!(!names.contains("count")); // a local
    assert!(!names.contains("layout"));
}
