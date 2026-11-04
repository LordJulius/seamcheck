//! Ruby's types from the project's own call sites (lang/ruby/infer.rs),
//! ported from tests/test_ruby.py's RubyResolutionTest.

mod common;

use std::collections::BTreeMap;

use seamcheck::graph::Graph;
use tempfile::TempDir;

fn load(files: &[(&str, &str)]) -> (TempDir, Graph) {
    let d = TempDir::new().unwrap();
    common::write(d.path(), files);
    common::write(d.path(), &[("seamcheck.toml", "[project]\nname = \"t\"\ninclude = [\"**\"]\n")]);
    let p = common::load(d.path());
    let g = Graph::load(&p);
    (d, g)
}

fn edges(g: &Graph, suffix: &str) -> BTreeMap<String, bool> {
    let ids: Vec<&String> = g.funcs.keys().filter(|f| f.ends_with(suffix)).collect();
    assert_eq!(ids.len(), 1, "{suffix}: {ids:?}");
    g.edges.get(ids[0]).map(|v| v.iter().map(|(t, _, e)| (t.clone(), *e)).collect()).unwrap_or_default()
}

fn want(pairs: &[(&str, bool)]) -> BTreeMap<String, bool> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

// test_ruby.py: RubyResolutionTest.test_a_constructor_parameter_has_the_type_every_caller_passes
#[test]
fn a_constructor_parameter_has_the_type_every_caller_passes() {
    // jekyll: `@reader = Reader.new(self)` in Site, `@site = site` in Reader,
    // `LayoutReader.new(site)` in Reader; each Drop wraps its own object
    let (_d, g) = load(&[
        ("lib/site.rb", "class Site\n  def initialize\n    @reader = Reader.new(self)\n  end\n  def config; end\nend\n"),
        (
            "lib/reader.rb",
            "class Reader\n  attr_reader :site\n  def initialize(site)\n    @site = site\n  end\n  def read\n    site.config\n    LayoutReader.new(site).read\n  end\nend\n",
        ),
        (
            "lib/layout_reader.rb",
            "class LayoutReader\n  def initialize(site)\n    @site = site\n  end\n  def read\n    @site.config\n  end\nend\n",
        ),
        (
            "lib/document.rb",
            "class Document\n  def initialize(path, relations = {})\n    @site = relations[:site]\n  end\n  def write\n    @site.config\n  end\n  def to_liquid\n    DocumentDrop.new(self)\n  end\nend\n",
        ),
        (
            "lib/drops.rb",
            "class Drop\n  def initialize(obj)\n    @obj = obj\n  end\nend\nclass DocumentDrop < Drop\n  def collection\n    @obj.write\n  end\nend\nclass SiteDrop < Drop\n  def config\n    @obj.config\n  end\nend\n",
        ),
        (
            "lib/use.rb",
            "class Use\n  def go(site)\n    SiteDrop.new(Site.new)\n    Document.new(\"p\", :site => Site.new)\n  end\nend\n",
        ),
    ]);
    assert_eq!(edges(&g, "/reader.Reader.read")["lib/site.Site.config"], true);
    assert_eq!(edges(&g, "LayoutReader.read"), want(&[("lib/site.Site.config", true)]));
    assert_eq!(edges(&g, "Document.write"), want(&[("lib/site.Site.config", true)]));
    assert_eq!(edges(&g, "DocumentDrop.collection"), want(&[("lib/document.Document.write", true)]));
    assert_eq!(edges(&g, "SiteDrop.config"), want(&[("lib/site.Site.config", true)]));
}

// test_ruby.py: RubyResolutionTest.test_parameters_typed_by_every_caller_and_includers_that_agree
#[test]
fn parameters_typed_by_every_caller_and_includers_that_agree() {
    // jekyll's Doctor: `healthy?(site)` with a Site; Convertible's `site` is Page#site and
    // Layout#site, both a Site; `Generator = Class.new(Plugin)`
    let (_d, g) = load(&[
        ("lib/site.rb", "class Site\n  def config; end\nend\n"),
        (
            "lib/doctor.rb",
            "class Doctor\n  def process\n    site = Site.new\n    healthy?(site)\n  end\n  def healthy?(site)\n    site.config\n  end\nend\n",
        ),
        ("lib/convertible.rb", "module Convertible\n  def read\n    site.config\n  end\nend\n"),
        (
            "lib/page.rb",
            "class Page\n  include Convertible\n  attr_reader :site\n  def initialize(site)\n    @site = site\n  end\nend\nclass Layout\n  include Convertible\n  attr_reader :site\n  def initialize(site)\n    @site = site\n  end\nend\nclass Build\n  def go\n    s = Site.new\n    Page.new(s)\n    Layout.new(s)\n  end\nend\n",
        ),
        ("lib/plugin.rb", "class Plugin\n  def self.priority(p = nil); end\nend\nGenerator = Class.new(Plugin)\n"),
        ("lib/pagination.rb", "class Pagination < Generator\n  priority :high\nend\n"),
    ]);
    assert_eq!(edges(&g, "Doctor.healthy?"), want(&[("lib/site.Site.config", true)]));
    assert_eq!(edges(&g, "Convertible.read")["lib/site.Site.config"], true);
    assert_eq!(edges(&g, "Pagination.<top>"), want(&[("lib/plugin.Plugin.priority", true)]));
}

// test_ruby.py: RubyResolutionTest.test_what_a_ruby_library_yields_and_what_isnt_a_call
#[test]
fn what_a_ruby_library_yields_and_what_isnt_a_call() {
    let (_d, g) = load(&[
        ("lib/doc.rb", "class Doc\n  def write; end\n  def stop!; end\n  def quit!; end\nend\n"),
        (
            "lib/use.rb",
            "class Use\n  def go(pages)\n    File.open(\"x\", \"w\") do |f|\n      f.write\n    end\n    Parallel.map(pages) { |page| page.write }\n    output, warnings = convert\n    output.strip\n  rescue ArgumentError => e\n    e.message\n  end\n  alias stop! quit!\n  def convert; end\nend\n",
        ),
    ]);
    let got = edges(&g, "Use.go");
    assert!(!got.contains_key("lib/doc.Doc.quit!"), "an alias names methods; it calls none");
    // `f` is Ruby's File; `page` may be ours (Parallel was handed our pages): a guess
    assert_eq!(got, want(&[("lib/use.Use.convert", true), ("lib/doc.Doc.write", false)]));
}
