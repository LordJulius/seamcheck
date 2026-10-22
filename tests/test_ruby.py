"""The Ruby adapter (python3 -m unittest discover tests). Each case is a shape
cut down from sinatra or jekyll (October 2026) that was misread, a name guess
or a wrong edge before the adapter handled it."""
import os
import sys
import tempfile
import textwrap
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core, lang_ruby  # noqa: E402
from seamcheck.scan import blank  # noqa: E402


def load(files: dict):
    d = tempfile.TemporaryDirectory()
    for name, body in files.items():
        path = os.path.join(d.name, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as fh:
            fh.write(textwrap.dedent(body))
    cfg = os.path.join(d.name, "seamcheck.toml")
    with open(cfg, "w") as fh:
        fh.write('[project]\nname = "t"\ninclude = ["**"]\n')
    return d, core.load(d.name, cfg, use_cache=False)


def edges(p, caller_suffix: str) -> dict:
    """{callee id: exact} for the one function whose id ends with caller_suffix."""
    [fid] = [f for f in p.funcs if f.endswith(caller_suffix)]
    return {t: ex for t, _, ex in p.edges.get(fid, [])}


def parse(text: str, rel: str = "lib/a.rb"):
    return lang_ruby.parse(rel, rel, textwrap.dedent(text))


def spans(mods) -> dict:
    return {f"{f.module.split('.', 1)[-1]}.{f.name}": (f.start, f.end) for m in mods for f in m.funcs if f.kind != "top"}


class RubyLiteralsTest(unittest.TestCase):
    def test_a_literal_hides_its_keywords_and_keeps_interpolated_calls(self):
        # a heredoc / %w() / regex / =begin holding `end` or a quote shifted
        # every block after it; `"#{foo("b")}"` ended the string at the inner quote
        src = textwrap.dedent('''\
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
            ''')
        code, strings = blank(src, "ruby")
        self.assertEqual(len(code), len(src))
        lines = code.split("\n")
        self.assertIn('foo(" ")', lines[0])
        self.assertNotIn("end", lines[0] + lines[1] + lines[3] + lines[5] + lines[7])
        self.assertIn("bar(1)", lines[4])
        self.assertIn("q = a / b", lines[7])  # a division, not a regex
        self.assertNotIn("hidden", code)
        self.assertIn(("def end class"), [s for _, s in strings])

    def test_other_languages_blank_as_before(self):
        self.assertEqual(blank("x = 'a' # c\n", "python")[0], "x = ' '    \n")


class RubyBlocksTest(unittest.TestCase):
    def test_modifiers_operands_and_loops_open_the_right_blocks(self):
        # sinatra's `host << if request.forwarded?` closed the module early;
        # `return a || /.*/ unless b` (a regex before `unless`) opened one
        mods = parse('''\
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
            ''')
        got = spans(mods)
        self.assertEqual(got["Sinatra::Helpers.uri"], (3, 14))
        self.assertEqual(got["Sinatra::Helpers.prefixed_path"], (16, 19))
        self.assertEqual(got["Sinatra::Helpers.size"], (21, 21))  # endless: no `end`
        self.assertEqual(got["Sinatra::Helpers.last"], (26, 26))
        self.assertEqual({m.name for m in mods}, {"lib/a", "lib/a.Sinatra", "lib/a.Sinatra::Helpers"})

    def test_rspec_minitest_and_shoulda_blocks_are_tests(self):
        mods = parse('''\
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
            ''', "test/foo_test.rb")
        tests = {f.name for m in mods for f in m.funcs if f.kind == "test"}
        self.assertEqual(tests, {"test does a thing", "test_bar", "test baz works", "test qux"})
        calls = {c.name for m in mods for f in m.funcs for c in f.calls}
        self.assertNotIn("context", calls)  # the framework's grouping, not a project call
        self.assertNotIn("it", calls)


class RubyCallsTest(unittest.TestCase):
    def test_calls_need_no_parentheses_and_locals_setters_keys_are_not_calls(self):
        mods = parse('''\
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
            ''')
        f = next(f for m in mods for f in m.funcs if f.name == "process")
        calls = {(c.name, c.chain) for c in f.calls}
        self.assertIn(("build_reader", ()), calls)
        self.assertIn(("read", ("reader",)), calls)
        self.assertIn(("render", ()), calls)
        self.assertIn(("payload", ()), calls)  # a bare name nobody assigned: a method call
        names = {c.name for c in f.calls}
        self.assertNotIn("config", names)  # a parameter, and `self.config =` is a setter
        self.assertNotIn("json", names)  # a hash key
        self.assertNotIn("count", names)  # a local
        self.assertNotIn("layout", names)


class RubyResolutionTest(unittest.TestCase):
    def test_classes_reopened_across_files_mixins_and_singletons(self):
        d, p = load({
            "lib/jekyll.rb": """\
                module Jekyll
                  def self.logger
                    @logger ||= LogAdapter.new(Stevenson.new)
                  end
                end
                """,
            "lib/jekyll/log_adapter.rb": """\
                module Jekyll
                  class LogAdapter
                    def warn(msg); end
                  end
                end
                """,
            "lib/jekyll/utils.rb": """\
                module Jekyll
                  module Utils
                    extend self
                    def slugify(s) = s
                  end
                end
                """,
            "lib/jekyll/convertible.rb": """\
                module Jekyll
                  module Convertible
                    def render_all
                      output_ext
                    end
                  end
                end
                """,
            "lib/jekyll/page.rb": """\
                module Jekyll
                  class Page
                    include Convertible
                    def output_ext; end
                    def run
                      render_all
                      Utils.slugify("x")
                      Jekyll.logger.warn "x"
                    end
                  end
                end
                """,
            "lib/jekyll/page_ext.rb": """\
                module Jekyll
                  class Page
                    def reopened; end
                  end
                end
                """,
            "lib/jekyll/other.rb": """\
                module Jekyll
                  class Other
                    def warn; end
                    def go(page)
                      Page.new.reopened
                    end
                  end
                end
                """,
        })
        with d:
            self.assertEqual(edges(p, "Jekyll::Page.run"), {
                "lib/jekyll/convertible.Jekyll::Convertible.render_all": True,  # an included module's method
                "lib/jekyll/utils.Jekyll::Utils.slugify": True,  # `Utils` from inside module Jekyll
                "lib/jekyll.Jekyll.logger": True,  # `def self.logger`
                "lib/jekyll/log_adapter.Jekyll::LogAdapter.warn": True,  # its `@logger ||= LogAdapter.new` type
            })
            # a mixin's bare call reaches the class including it
            self.assertEqual(edges(p, "Jekyll::Convertible.render_all"), {"lib/jekyll/page.Jekyll::Page.output_ext": True})
            # a method defined where the class is reopened is the class's
            self.assertEqual(edges(p, "Jekyll::Other.go"), {"lib/jekyll/page_ext.Jekyll::Page.reopened": True})

    def test_requires_reach_top_level_methods_and_libraries_reach_nothing(self):
        d, p = load({
            "lib/sinatra/main.rb": """\
                require_relative "helpers"
                require "sinatra/base"
                def run_app
                  helper_fn
                  base_fn
                  puts "x"
                  Liquid::Template.parse("x")
                  data = File.read("x")
                  data.parse
                end
                """,
            "lib/sinatra/helpers.rb": "def helper_fn; end\n",
            "lib/sinatra/base.rb": "def base_fn; end\n",
            "lib/sinatra/parser.rb": "class Parser\n  def parse; end\nend\n",
        })
        with d:
            self.assertEqual(edges(p, "main.run_app"), {"lib/sinatra/helpers.helper_fn": True,
                                                        "lib/sinatra/base.base_fn": True})
            # a gem's constant and what a library returns reach nothing, not Parser#parse
            self.assertEqual(p.resolution[".rb"][1], 0)

    def test_a_local_built_by_new_and_a_field_set_in_initialize_have_their_types(self):
        d, p = load({
            "lib/app.rb": """\
                class Store
                  def save; end
                  def tap_me
                    self
                  end
                end

                class Other
                  def save; end
                end

                class App
                  attr_reader :store

                  def initialize
                    @store = Store.new
                  end

                  def run
                    s = Store.new(1)
                    s.save
                    @store.save
                    store.tap_me.save
                  end
                end
                """,
        })
        with d:
            # `Store.new(1)` runs no project code (Store has no initialize);
            # `s`, `@store` and `store` (attr_reader) are Stores; `tap_me` returns self
            self.assertEqual(edges(p, "App.run"), {"lib/app.Store.save": True, "lib/app.App.store": True,
                                                   "lib/app.Store.tap_me": True})

    def test_yard_and_sorbet_state_return_types(self):
        d, p = load({
            "lib/app.rb": """\
                class Repo
                  def find; end
                end

                class Svc
                  # @return [Repo]
                  def repo
                    @things.first
                  end

                  sig { returns(Repo) }
                  def other_repo
                    lookup
                  end

                  # @param r [Repo]
                  def use(r)
                    repo.find
                    other_repo.find
                    r.find
                  end
                end
                """,
        })
        with d:
            self.assertEqual(edges(p, "Svc.use"), {"lib/app.Svc.repo": True, "lib/app.Svc.other_repo": True,
                                                    "lib/app.Repo.find": True})

    def test_a_constructor_parameter_has_the_type_every_caller_passes(self):
        # jekyll: `@reader = Reader.new(self)` in Site, `@site = site` in Reader,
        # `LayoutReader.new(site)` in Reader; each Drop wraps its own object
        d, p = load({
            "lib/site.rb": """\
                class Site
                  def initialize
                    @reader = Reader.new(self)
                  end
                  def config; end
                end
                """,
            "lib/reader.rb": """\
                class Reader
                  attr_reader :site
                  def initialize(site)
                    @site = site
                  end
                  def read
                    site.config
                    LayoutReader.new(site).read
                  end
                end
                """,
            "lib/layout_reader.rb": """\
                class LayoutReader
                  def initialize(site)
                    @site = site
                  end
                  def read
                    @site.config
                  end
                end
                """,
            "lib/document.rb": """\
                class Document
                  def initialize(path, relations = {})
                    @site = relations[:site]
                  end
                  def write
                    @site.config
                  end
                  def to_liquid
                    DocumentDrop.new(self)
                  end
                end
                """,
            "lib/drops.rb": """\
                class Drop
                  def initialize(obj)
                    @obj = obj
                  end
                end
                class DocumentDrop < Drop
                  def collection
                    @obj.write
                  end
                end
                class SiteDrop < Drop
                  def config
                    @obj.config
                  end
                end
                """,
            "lib/use.rb": """\
                class Use
                  def go(site)
                    SiteDrop.new(Site.new)
                    Document.new("p", :site => Site.new)
                  end
                end
                """,
        })
        with d:
            self.assertEqual(edges(p, "/reader.Reader.read")["lib/site.Site.config"], True)
            self.assertEqual(edges(p, "LayoutReader.read"), {"lib/site.Site.config": True})
            self.assertEqual(edges(p, "Document.write"), {"lib/site.Site.config": True})
            self.assertEqual(edges(p, "DocumentDrop.collection"), {"lib/document.Document.write": True})
            self.assertEqual(edges(p, "SiteDrop.config"), {"lib/site.Site.config": True})

    def test_a_core_method_on_an_untyped_receiver_is_rubys(self):
        # `opts.each`, `name.to_s`: not a guess at every project class with an
        # `each` (jekyll's Drop#each, Cache#key?); stats still counts it as named
        d, p = load({
            "lib/drop.rb": "class Drop\n  def each; end\n  def key?(k); end\nend\n",
            "lib/use.rb": """\
                class Use
                  def go(opts, name)
                    opts.each { |x| x }
                    opts.key?(:a)
                    name.to_s
                    opts.custom_thing
                  end
                end
                """,
            "lib/other.rb": "class Other\n  def custom_thing; end\nend\nclass Third\n  def custom_thing; end\nend\n",
        })
        with d:
            got = edges(p, "Use.go")
            self.assertNotIn("lib/drop.Drop.each", got)
            self.assertNotIn("lib/drop.Drop.key?", got)
            self.assertEqual(got, {"lib/other.Other.custom_thing": False, "lib/other.Third.custom_thing": False})
            self.assertEqual(p.resolution[".rb"][3], 2)  # each, key?: named, not hidden

    def test_a_library_calls_result_is_the_librarys(self):
        # Rack::Test's `last_response.body` landed on every project `body`
        d, p = load({
            "lib/app.rb": "class App\n  def body; end\nend\n",
            "spec/app_spec.rb": """\
                describe App do
                  let(:app) { App.new }
                  it "works" do
                    get "/"
                    last_response.body
                    app.body
                  end
                end
                """,
        })
        with d:
            self.assertEqual(edges(p, "test works"), {"spec/app_spec.app": True, "lib/app.App.body": True})

    def test_an_include_inside_a_describe_block_stays_there(self):
        # sinatra's helpers_test: `describe 'mime_type' do include Sinatra::Helpers`
        # made every `status` in the file Helpers#status
        d, p = load({
            "lib/helpers.rb": "module Helpers\n  def status; end\n  def mime_type; end\nend\n",
            "test/helpers_test.rb": """\
                class HelpersTest < Minitest::Test
                  describe "mime_type" do
                    include Helpers
                  end

                  it "conditional GET" do
                    status
                  end
                end
                """,
        })
        with d:
            self.assertEqual(edges(p, "test conditional GET"), {})

    def test_top_level_include_and_a_reopened_library_class(self):
        # jekyll's test helper: `include Jekyll` at the top, so `Site` is
        # Jekyll::Site everywhere; sinatra's: `class Minitest::Test` adds mock_app
        d, p = load({
            "lib/jekyll/site.rb": "module Jekyll\n  class Site\n    def process; end\n  end\nend\n",
            "test/helper.rb": """\
                include Jekyll
                class Minitest::Test
                  def mock_app; end
                end
                """,
            "test/test_site.rb": """\
                require "helper"
                class TestSite < Minitest::Test
                  def test_process
                    Site.new.process
                    mock_app
                  end
                end
                """,
        })
        with d:
            self.assertEqual(edges(p, "TestSite.test_process"), {
                "lib/jekyll/site.Jekyll::Site.process": True, "test/helper.Minitest::Test.mock_app": True})

    def test_shoulda_context_setup_types_its_tests_fields(self):
        d, p = load({
            "lib/post.rb": "class Post\n  def excerpt; end\nend\nclass Page\n  def excerpt; end\nend\n",
            "test/test_post.rb": """\
                class TestPost < Minitest::Test
                  context "a post" do
                    setup do
                      @post = Post.new
                    end
                    should "have an excerpt" do
                      @post.excerpt
                    end
                  end
                  context "a page" do
                    setup do
                      @post = Page.new
                    end
                    should "also have one" do
                      @post.excerpt
                    end
                  end
                end
                """,
        })
        with d:
            self.assertEqual(edges(p, "test have an excerpt"), {"lib/post.Post.excerpt": True})
            self.assertEqual(edges(p, "test also have one"), {"lib/post.Page.excerpt": True})

    def test_a_library_class_the_project_only_adds_to_stays_the_librarys(self):
        # sinatra reopens `class Rack::Builder` (lib and test helper) and a test
        # subclasses it: `builder.use` in lib is Rack's, not the test's `use`
        d, p = load({
            "lib/main.rb": "class Rack::Builder\n  include Delegator\nend\n",
            "test/test_helper.rb": "class Rack::Builder\n  def include?(m); end\nend\n",
            "test/settings_test.rb": """\
                class SettingsTest < Minitest::Test
                  class MiddlewareTracker < Rack::Builder
                    def use(m); end
                    def check
                      include?(1)
                    end
                  end
                end
                """,
            "lib/base.rb": """\
                class Base
                  def build
                    builder = Rack::Builder.new
                    setup builder
                  end
                  def setup(builder)
                    builder.use 1
                  end
                end
                """,
        })
        with d:
            self.assertEqual(edges(p, "Base.setup"), {})
            # the added method still reaches the subclass
            self.assertEqual(edges(p, "MiddlewareTracker.check"), {"test/test_helper.Rack::Builder.include?": True})

    def test_parameters_typed_by_every_caller_and_includers_that_agree(self):
        # jekyll's Doctor: `healthy?(site)` with a Site; Convertible's `site`
        # is Page#site and Layout#site, both a Site; `Generator = Class.new(Plugin)`
        d, p = load({
            "lib/site.rb": "class Site\n  def config; end\nend\n",
            "lib/doctor.rb": """\
                class Doctor
                  def process
                    site = Site.new
                    healthy?(site)
                  end
                  def healthy?(site)
                    site.config
                  end
                end
                """,
            "lib/convertible.rb": """\
                module Convertible
                  def read
                    site.config
                  end
                end
                """,
            "lib/page.rb": """\
                class Page
                  include Convertible
                  attr_reader :site
                  def initialize(site)
                    @site = site
                  end
                end
                class Layout
                  include Convertible
                  attr_reader :site
                  def initialize(site)
                    @site = site
                  end
                end
                class Build
                  def go
                    s = Site.new
                    Page.new(s)
                    Layout.new(s)
                  end
                end
                """,
            "lib/plugin.rb": """\
                class Plugin
                  def self.priority(p = nil); end
                end
                Generator = Class.new(Plugin)
                """,
            "lib/pagination.rb": "class Pagination < Generator\n  priority :high\nend\n",
        })
        with d:
            self.assertEqual(edges(p, "Doctor.healthy?"), {"lib/site.Site.config": True})
            got = edges(p, "Convertible.read")
            self.assertEqual(got["lib/site.Site.config"], True)
            self.assertEqual(edges(p, "Pagination.<top>"), {"lib/plugin.Plugin.priority": True})

    def test_what_a_ruby_library_yields_and_what_isnt_a_call(self):
        d, p = load({
            "lib/doc.rb": "class Doc\n  def write; end\n  def stop!; end\n  def quit!; end\nend\n",
            "lib/use.rb": """\
                class Use
                  def go(pages)
                    File.open("x", "w") do |f|
                      f.write
                    end
                    Parallel.map(pages) { |page| page.write }
                    output, warnings = convert
                    output.strip
                  rescue ArgumentError => e
                    e.message
                  end
                  alias stop! quit!
                  def convert; end
                end
                """,
        })
        with d:
            got = edges(p, "Use.go")
            self.assertNotIn("lib/doc.Doc.quit!", got)  # an alias names methods; it calls none
            # `f` is Ruby's File; `page` may be ours (Parallel was handed our pages): a guess
            self.assertEqual(got, {"lib/use.Use.convert": True, "lib/doc.Doc.write": False})

    def test_an_untyped_receiver_is_a_guess_unless_named_after_its_class(self):
        d, p = load({
            "lib/app/site.rb": """
                module App
                  class Site
                    def render; end
                  end
                end
            """,
            "lib/app/log.rb": """
                module App
                  class Log
                    def deliver(x); end
                  end
                end
            """,
            "lib/app/page.rb": """
                module App
                  class Page
                    def initialize(site, app)
                      @site = site
                      @app = app
                    end

                    def go
                      @site.render
                      @app.deliver(1)
                    end
                  end
                end
            """,
        })
        got = edges(p, "Page.go")
        self.assertTrue(got.get("lib/app/site.App::Site.render"))  # `@site`: by convention a Site
        self.assertFalse(got.get("lib/app/log.App::Log.deliver"))  # `@app`: the only `deliver`, yet no proof
        d.cleanup()


class RubyCollectionsTest(unittest.TestCase):
    """What a collection holds (`docs << doc`, `hsh[name] = Collection.new`),
    read back by an index, `first` / `detect`, or an iterator's block."""

    @classmethod
    def setUpClass(cls):
        cls.d, cls.p = load({
            "lib/site.rb": """\
                class Site
                  def collections
                    @collections ||= names.each_with_object({}) do |name, hsh|
                      hsh[name] = Collection.new(self, name)
                    end
                  end

                  def names
                    []
                  end
                end
                """,
            "lib/collection.rb": """\
                class Collection
                  def initialize(site, label)
                    @site = site
                  end

                  def docs
                    @docs ||= []
                  end

                  def read(path)
                    doc = Document.new(path)
                    docs << doc
                  end

                  def paths
                    docs.sort.each do |doc|
                      doc.relative_path
                    end
                  end
                end
                """,
            "lib/document.rb": """\
                class Document
                  def initialize(path)
                    @path = path
                  end

                  def relative_path
                    @path
                  end
                end
                """,
            "lib/page.rb": """\
                class Page
                  def relative_path
                    "x"
                  end
                end
                """,
            "lib/use.rb": """\
                class Use
                  def first_doc(site)
                    doc = Site.new.collections["posts"].docs.detect { |d| d }
                    doc.relative_path
                  end
                end
                """,
        })

    @classmethod
    def tearDownClass(cls):
        cls.d.cleanup()

    def test_an_iterator_block_param_is_an_element(self):
        self.assertEqual(edges(self.p, "Collection.paths").get("lib/document.Document.relative_path"), True)

    def test_a_hash_of_built_values_and_a_pushed_array(self):
        self.assertEqual(edges(self.p, "Use.first_doc").get("lib/document.Document.relative_path"), True)


class RubyBlocksAndSpecsTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.d, cls.p = load({
            "lib/base.rb": """\
                class Base
                  def initialize(app)
                    @app = app
                  end

                  def referrer(env)
                    env
                  end
                end
                """,
            "lib/io.rb": """\
                class Writer
                  def write(x)
                    x
                  end
                end
                """,
            "spec/base_spec.rb": """\
                RSpec.describe Base do
                  subject { described_class.new(-> {}) }

                  def update_file(path, &block)
                    File.open(path, "w", &block)
                  end

                  it "reads the referrer" do
                    expect(subject.referrer({})).to eq({})
                  end

                  it "writes" do
                    update_file("a") do |f|
                      f.write "x"
                    end
                  end
                end
                """,
        })

    @classmethod
    def tearDownClass(cls):
        cls.d.cleanup()

    def test_described_class(self):
        self.assertEqual(edges(self.p, "reads the referrer").get("lib/base.Base.referrer"), True)

    def test_a_block_handed_on_to_file_open_is_files(self):
        self.assertNotIn("lib/io.Writer.write", edges(self.p, "writes"))


if __name__ == "__main__":
    unittest.main()
