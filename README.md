# seamcheck

See a whole codebase at once and check the contracts between its parts.

Most bugs that survive review live in the seams: a client that renders a panel
type the server stopped sending, a route nobody calls, a QML file that reads an
empty object on one Qt build, a test that passes without touching the code it
names. No compiler sees them, because each side is fine on its own. seamcheck
builds one call graph across every language in a repo, then checks the rules
the project writes down about how its parts fit together.

- **Deterministic and local.** No network, no model, no dependencies beyond
  Python 3.11+ (ripgrep is used when present).
- **Built for agents and CI.** Plain text, JSON, SARIF (code scanning) or
  GitHub annotations; exit code 1 on errors; a pre-commit gate; queries (`callers`, `impact`, `refs`) that replace grep
  loops and say how sure they are; and an MCP server that keeps the graph loaded.
- **Many languages, one graph.** Elixir, Kotlin, Java, TypeScript/JavaScript
  (JSX), C, C++, QML, Python, GDScript, Rust, Go, C#, Swift, Ruby and PHP.
  Rules work on any text file.
- **Says how sure it is.** On 37 open-source repos in 16 languages, 95% or
  more of the calls into project code are exact on 29, and 90% on 36 (see
  "How exact it is"). The rest are marked as guesses, never passed off as fact.
- **Every check came from a real bug.** Each rule and lint was written after a
  bug it would have caught, and its test reproduces that bug (cut down) and
  its fix.

Speed, on a 16-core machine: a 133-file Phoenix + TypeScript game loads in
0.5 s cold and 0.1 s warm. A 4,100-file monorepo (Phoenix hub, Android, Qt and
React clients; 43k functions in seven languages) loads in about 16 s cold and
1.8 s warm; `gate` on a small change takes about 2 s, and that repo's 122
rules run in about 24 s, most of it the project's own command rules.

## Quick start

```sh
pipx install git+https://github.com/LordJulius/seamcheck   # or, from a checkout: pip install --user .
```

Or run it from a checkout with no install:
`PYTHONPATH=/path/to/seamcheck python3 -m seamcheck check`.

Add a `seamcheck.toml` at the root of your repo:

```toml
[project]
name = "My app"
include = ["lib/**", "web/src/**", "android/**/*.kt"]

[[rule]]
id = "damage-goes-through-take-hit"
kind = "forbid"
pattern = '\.hp\s*-='
in = ["game/**"]
allow = ["game/entities/Unit.gd:take_hit"]
why = "damage outside take_hit skips armor, shields and crits"
```

Or let seamcheck write a first one from what the repo holds (its languages,
the folders their code lives in, the rule packs of the frameworks it finds,
one commented example of each common rule kind), and install the pre-commit
gate:

```sh
seamcheck init --hook
```

Then:

```sh
seamcheck check                 # every rule; exit 1 on errors
seamcheck stats                 # what the graph sees, per language
seamcheck callers take_hit      # who calls it
seamcheck gate --staged         # in a pre-commit hook: may this change land?
```

## Commands

```
Starting
  seamcheck init [--hook] [--force] a first seamcheck.toml for this repo (+ packs, + the pre-commit gate)
  seamcheck serve                   an MCP server (stdio): the queries below from one loaded graph

Checks
  seamcheck check [--format F] [--only id,id] [--skip id,id] [--errors-only]
                                    run every rule (exit 1 on errors)
  seamcheck gate [--since REF] [--staged] [--format F]
                                    may this change land? (see "The land gate")
  seamcheck tighten                 run each command rule's `tighten` (an allowlist rewrite that only shrinks)

The call graph
  seamcheck callers NAME [--depth N]   who calls it, transitively (overrides included)
  seamcheck callees SPEC [--depth N]   what it calls
  seamcheck path FROM TO               the shortest call chain between two functions
  seamcheck find NAME                  where functions are defined (+ caller counts)
  seamcheck refs NAME...               defined / called / mentioned anywhere (scripts and config too)
  seamcheck impact TARGET... [--depth N] [--since REF]
                                       what a change could break: callers + bridged views, by area
  seamcheck dead                       functions nothing calls or references (a hint, not proof)
  seamcheck stats                      files, functions, edges (exact vs fuzzy), timing, and per
                                       language how many calls into project code are exact
  seamcheck map --html out.html        interactive map (for humans)

Keeping a repo tidy
  seamcheck tidy [--since REF]         one compact report: biggest files, copies, dead code, test value
  seamcheck unused / test-only / tests / dups / hotspots / split FILE
  seamcheck remove ID... [--apply]     delete functions or test blocks with their docs (dry run by default)

Contracts across views and dependencies
  seamcheck matrix [RULE] [--all]      a fanout rule's token x view grid
  seamcheck deps [NAME] [--all] [--json]   declared dependencies vs use

Runtime (the program reports itself; see "Runtime checks")
  seamcheck trace LOG...               check finished runs' output ('-' = stdin)
  seamcheck run [--log F] [--timeout S] -- CMD...
  seamcheck fuzz --seeds 1-8 [--jobs 4] -- CMD... {seed} ...

Options
  -C DIR          project root (default: the nearest parent with seamcheck.toml)
  --config PATH   more config: a .toml or a folder of them (repeatable)
  --fuzzy         callers / callees / path / impact: also walk name-guess edges
  --precise       ask language servers where the guessed calls go (see "Precise mode")
  --no-cache      parse everything again
  --strict        runtime: also flag rules that had nothing to check in this run
  --format F      check / gate: text (default), json (= --json), sarif, github
  --version
```

SPEC is `name` (glob ok), `Module.name`, or `path/glob:funcglob`.

## The call graph

Every adapter turns files into modules, functions and calls. seamcheck then
links each call to the function it reaches.

An edge is **exact** when the target is certain:

- the same module, a parent class (and its subclasses' overrides), an alias,
  a preload, or a qualified module name;
- a name imported by the file: Elixir `alias A.{B, C}`, `import A` and the
  imports an Elixir `use` injects (its `__using__` or `using do` block, or
  `def controller` for `use App, :controller`); JS/TS `import {f as g}`,
  `* as ns`, default imports, an object imported by name (`API.create()`),
  tsconfig `paths` (`@/lib/x`, an exact `"@app/common"`, an `extends`ed base's)
  and workspace packages by their package.json name; Kotlin/Java
  same-package files, `import a.b.fn` and `import a.b.*`; C# the file's
  namespace and its `using`s; Python `import a.b as c` and `from .x import y`
  (absolute imports find the package under its source root: `src/flask` over
  a test fixture named `flask.py`); Go the files of the same directory and
  package, and imports through `go.mod`; Rust `use` trees through `crate::`,
  `self::`, `super::`, child modules and the workspace's crates (a
  `[[bin]] path` root included);
- a name a barrel or package re-exports: `export * from`,
  `export { a as b } from`, an `__init__.py`'s imports and `import *`, Rust
  `pub use`;
- a variable whose type the code states or builds: Kotlin `val r: Repo`, TS
  `const s: Store`, parameters, class fields and constructor properties, C++
  `Type* p`, `Type& r`, `Type v(...)` and a class's header members for its
  `.cpp`, Java/C# `Repo repo` and fields, Python `x: Repo` and
  `self.repo = Repo()`, Go receivers, parameters, struct fields and
  `x := &T{}` (`w tui.Window` too), Rust `x: &mut Repo`, and a constructor
  (`val prefs = Prefs(ctx)`, `const hub = new Hub()`, `let s = Searcher::new()`);
- `self.x()` / `this.x()` / `Self::x()` / `super().x()`: the caller's own class
  (a Python class is a module of its own, `file.Class`, its parent the first
  base; a Go type's methods are the module `file.Type`), and `Foo()` in Python
  is `Foo.__init__`;
- an object C++ hands to QML by name (`setContextProperty("continuity",
  &continuity)`), typed by that variable's declaration;
- a C++ namespace (`veridian::api::parseClaim()` reaches the one in `api/`);
- in Ruby (see "The Ruby adapter" below): a constant as Ruby looks it up (the
  lexical scopes, the class's ancestors, a top-level `include`), a class
  reopened in several files as one class, `< Base` and `include` / `extend`
  as supertypes (a mixin's bare call reaches the classes including it),
  `def self.x` / `class << self` / `extend self` as `A.x`, `Foo.new` as
  `Foo#initialize`, `require` / `require_relative` for top-level methods,
  and the types of locals, fields and return values (below);
- a helper the calling function defines itself (`const nudge = () => ...`
  inside a component);
- a Kotlin extension the caller's own file declares (it shadows other files'
  of the same name);
- a `[project.groups]` binding (below);
- a receiver typed step by step (`self.store.get().save()`): a field's or
  property's declared type, what a method returns (`-> Foo`, `: Foo`, its own
  last `return x`, a builder step returning `this`), a getter (`get x()`,
  `@property`), a cast (`(x as Foo).y()`), a closure's body, an overload set
  that all returns one type; and the parameter a framework hands a function,
  as its pack says (Express's `(req, res)`, Hono's `c`; "Framework packs");
- Swift's implicit members (`.status(404)`, `case .value(let v)`), typed by
  their context: the parameter they're passed to (overloads settle by which
  parameter type has the member), a `return`, `self =`, a typed `let` or an
  array element; a pattern calls nothing;
- GDScript's `$Node`: the node of that name in the `.tscn` scenes whose root
  runs the script (a scripted or instanced node is its script's class);
- a C++ class QML knows by another name (`QML_NAMED_ELEMENT(Phone)` +
  `QML_FOREIGN(PhoneImpl)`), and a root object's property seen from the
  components it instantiates (QML's dynamic scope).

An edge is **fuzzy** (a name guess) when the call is on a receiver of unknown
type: every function with that name is a candidate, unless more than 8 share
it (then it's dropped as noise). That holds even when only one module in scope
has a function of that name: sampled across hono, gson, Polly, okhttp and
excalidraw, those edges were often wrong (`res.json()` on a fetch Response is
not the project's `Context.json`), so a unique name alone is never "exact". Guesses are limited to what could be true: a
guess never crosses languages (Kotlin never calls TypeScript; QML may call its
JS libraries and C++), and product code is never guessed into a test file.

Some calls resolve to **nothing**, on purpose:

- a qualifier that names a module with no such function in the project
  (`Enum.reject`, `Repo.rollback`) is a library call;
- in Elixir, Kotlin, Java, TypeScript, Python, Go, Rust and Ruby a bare call with
  nothing in scope is the language's own library (`Enum`, Compose's `Row`,
  vitest's `expect`, `len`, `append`, `println!`);
- a call on a name imported from outside the project (`json.dumps()`,
  `fmt.Println()`, `_.map()` after `import _ from "lodash"`);
- a call on a variable typed outside the project (`s: String; s.split()`),
  unless a project Kotlin extension is declared on that type;
- a value the language or runtime owns: a literal, `x: Foo[]` / `x = []`,
  Python's builtins (`dict`, `vars(x)`, `**kwargs`) and a class whose base
  is one, JS globals (`console`, `document`, `vi`) and DOM properties
  (`el.classList`), Rust's primitives and the crates Cargo.toml depends on,
  a method missing from a type the project only extends (a Swift `extension
  Data`, a `global.d.ts` addition to `CanvasRenderingContext2D`), a TS type
  an ES module neither declares nor imports (`Response` is the runtime's);
- in Rust, `.unwrap()`, `.len()`, `.push()` and the other everyday
  Option / Result / Vec / String / iterator methods on a receiver of unknown
  type: they are the standard library's, not a project function of that name;
- in Ruby, the same for Object / Enumerable / Hash / Array / String methods
  (`each`, `to_s`, `key?`, `merge`...) on a receiver nothing types, and any
  call on a library's value: a literal (`"a".split`, `%w(a b).each`), a gem's
  constant (`Liquid::Template.parse`), what a bare call reaching no project
  method returns (Rack::Test's `last_response.body`), what Ruby's own
  classes yield (`File.open(p) do |f|`).

`seamcheck stats` prints, per language, how many call sites are exact, guessed
or nothing, and of the calls that reach project code, how many are exact.
Watch that number: a drop after a change means the tool lost an import. The
gate watches it for you (below).

Queries say how sure they are. `callers`, `callees`, `path` and `impact` walk
exact edges; `--fuzzy` adds the guesses. `find` and the `callers` header print
both counts (`callers: 12 exact, 1849 name-guess`).

Tests are functions too (kind `test`): ExUnit `test "..." do` (including a
head that goes on to the next line, like `test "x",\n %{conn: conn} do`),
vitest/jest `it(...)` / `test(...)`, JUnit `@Test`, pytest `test_*` functions
and `Test*` class methods in test files, Go `TestX` / `BenchmarkX` /
`ExampleX` / `FuzzX` in `_test.go`, Rust `#[test]` (and `#[tokio::test]`),
xUnit / NUnit / MSTest `[Fact]` / `[Test]` / `[TestMethod]`, and Ruby's RSpec
`it "..." do` / `specify`, Minitest `def test_x` and `test "x" do`, and
Shoulda `should "x" do` (named `test <label>`, in the class around them),
XCTest / PHPUnit `testX` methods and swift-testing `@Test` / PHPUnit
`#[Test]`.
So `callees 'test *'` says what a test exercises. ExUnit `setup` and
`describe`, and RSpec / Shoulda `describe`, `context`, `setup` and `before`,
are fixtures and grouping, not tests. Ruby test files are `*_test.rb`,
`*_spec.rb`, `test_*.rb` and anything under `test/` or `spec/`.

Adapter details: a Java / C# method's parameters may span lines and its body
may open on the next line (Allman braces), a C# body may be `=> expr;`; a
TypeScript class field holding an arrow function (`private onDown = (e) =>
{`) is a function; Python module-level code (decorators, `app = create()`,
`if __name__ == "__main__":`) belongs to the file's `<top>`; C++ out-of-line methods, `->` calls and `25'000` digit
separators; QML functions, `onX:` handlers and component instances (a use of
`X.qml`); JSX `<Card />` is a call to `Card`; an Elixir sigil (`~w(a b)`) is
not a call. Swift: a SwiftPM target (`Sources/<Target>/**`) is one scope and
`import X` brings target X in; extensions join their type (across targets),
protocols are interfaces, `init` is what `Foo(...)` runs. PHP: a file's
`namespace` is its package, `use A\B\C [as D]` (and `use A\{B, C}`) imports,
typed and promoted properties and `$this->x = $param` type fields, a trait's
methods are its users', `new Foo` runs `__construct`; `#` comments,
heredocs and `?>` HTML are blanked. A file an adapter crashes on is a
`config` error finding, never a silently empty file.

### How exact it is

`bench/bench.py REPOS_DIR` measures it on shallow clones of open-source repos:
per language and repo, the share of calls into project code that are exact
(`stats`' "project calls exact"), and "named-nothing", the calls that reached
nothing though a project function has that name (a resolver that turns
guesses into nothing raises the first number and shows in the second). Exact
edges are also checked by hand: random samples per language, each traced to
the code.

| language | repo: project calls exact (static, October 2026) |
|---|---|
| Python | flask 95.7%, httpx 95.6%, requests 95.5% |
| C | jq 99.9%, redis 100.0% |
| C++ | flameshot 96.5%, kdeconnect 95.8% |
| C# | Polly 95.2%, Newtonsoft.Json 95.5% |
| Java | gson 97.1%, jsoup 96.8% |
| Kotlin | nowinandroid 95.8%, okhttp 95.6% |
| Rust | ripgrep 95.5%, bat 95.3% |
| Go | cobra 95.9%, fzf 96.0% |
| TypeScript | excalidraw 95.2%, hono 95.5%, plausible 97.8% |
| JavaScript | express 95.3%, hexpm 97.8%, plausible 95.3% |
| Elixir | plausible 99.9%, hexpm 99.4%, phoenix 99.9% |
| PHP | Slim 98.1%, guzzle 95.6% |
| Swift | Alamofire 94.0%, swift-argument-parser 92.9% |
| Ruby | sinatra 94.0%, jekyll 92.9% |
| GDScript | godot-demo-projects 90.8%, tps-demo 92.9% |
| QML | neochat 93.7%, cool-retro-term 96.9%, kdeconnect **79.4%** |

What gets a call there, beyond imports and declared types: return types
through chains (`a.b().c()`), generic arity (C#'s `Policy` vs `Policy<T>`),
lambda and closure parameters typed from the function they're passed to
(`services.Add(b => b.AddRetry())`, `|m| m.start()`, `xs.map { $0.name }`),
for-loop and index elements of declared containers and returned sequences,
Rust `Option` / `Result` kept wrapped until unwrapped and enum variants in
match arms, Kotlin / C# extension functions, TS `declare global` augmentations
and JSX runtimes, and a callback parameter called as a function when every
caller passes the same project function. The rest stay guesses on purpose: a
duck-typed Ruby value that may be a `Document` or a `Page`, a Swift implicit
member whose context type isn't written down, a callback the callers don't
name.

kdeconnect's QML pages take their plugin interface as a `property QtObject`
that a factory fills in when the page is pushed, a different type per page:
no static reading can type it, and seamcheck leaves those calls as guesses
rather than pick one. Precise mode (below) asks a language server about the
remaining guesses.

### The Ruby adapter

`.rb`, `.rake` and `.ru` files. Each file is a module and each class or
module its own, `<file>.<A::B>` (`module A; class B` and `class A::B` both
make `A::B`). Blocks are read off `class` / `module` / `def` / `do` /
`begin` / `case` / `end`: an `if` / `unless` / `while` that leads a statement
or a value (`x = if c`, `host << if c`) opens one, a modifier (`return if c`)
does not, `while c do` is one block, `def x = expr` (endless) none; heredocs,
`%w()` / `%r{}` literals, regexes, `?"` and `=begin` blocks are blanked
(interpolated `#{code}` stays: its calls are calls).

A call needs no parentheses: `name(`, every `.name` that isn't a setter
(`obj.x = v`), and a bare name the method hasn't assigned before (a local, a
parameter, a block's `|x|`, a `rescue => e` is not a call; a hash key
`name:`, a symbol and `alias a b` aren't either). `super` calls the parent's
method of the same name. `attr_reader` / `attr_accessor`, `def_delegators`,
`delegate ... to:`, `define_method(:x)` and RSpec `let(:x)` / `subject` are
methods; `Foo = Class.new(Base)` / `Struct.new` is a class.

Ruby states few types, so they are read off what the code does:

- `x = Foo.new(...)`, `@x = Foo.new` (or `self.x =` through an accessor) in
  any method, including `nil` in one place and a Foo in another, and a test's
  fields from the `setup do` / `before do` of the contexts around it;
- a method's last expression (`Foo.new(...).tap { }`, `self`, `@x ||= ...`),
  YARD `@return [Foo]` / `@param x [Foo]` and Sorbet `sig { params(x:
  Foo).returns(Bar) }`;
- a parameter that every project call site passes the same type to
  (`Reader.new(self)` in Site makes `Reader#initialize(site)`'s `site`, and
  the `@site` it is stored in, a Site; `Document.new(path, :site => site)`
  types `@site = relations[:site]`). Product callers must all be typed; a
  test's call site (often a double) only counts when it agrees. Each
  subclass built through a shared `initialize` takes its own callers' type
  (each Drop wraps its own object). For a method other than `initialize`
  every call of that name in the project must resolve, its name must never be
  a symbol (`send(:x)`, callbacks), and it must not check what it got
  (`respond_to?`, `is_a?`, `case x`);
- a mixin's `site` when every class including it returns the same type.

A library class the project only adds to (`class Rack::Builder` with one
method, every declaration in that form, none with a superclass or an
`initialize`) is still a supertype for the methods added, but a value of
that type is the library's.

### `[project.groups]`: what a node found by group is

Godot code finds singletons by group, not by type:
`var main := get_tree().get_first_node_in_group("match")`, then
`main.hum_of(team)`. The project states which script a group holds:

```toml
[project.groups]
match = "game/Main.gd"
events = "game/systems/Events.gd"
```

Inside one function, `var NAME := <x.>get_tree().get_first_node_in_group("match")`
binds `NAME` to that script for the rest of the function, and `NAME.foo()` is
exact. A helper (`var main := _main()`) does not bind, a group not in the table
stays a guess, and a file that isn't in the project is a config problem. List
only groups exactly one script joins.

### Precise mode: a language server settles the guesses

The static resolver reads names; it can't know what `shapes["c"].area()` or
`make().run()` is. With `--precise` (any command that loads the graph) or
`[precise] enabled = true`, each call it guessed, and each call that reached
nothing though a project function has the name, goes to the language's server
as a definition request:

- an answer in the project makes the call one exact edge (the function there,
  a class's constructor, and like any exact edge its subclasses' overrides);
- an answer outside it (the standard library, a dependency, `node_modules`)
  makes the guesses nothing: a library call;
- no answer, or one that isn't a function seamcheck knows (a parameter holding
  a callback, an interface's signature, a derive), leaves the call as it was.

Static resolution is the default and doesn't change. Precise mode only replaces
a guess with what a server says, and `seamcheck stats --precise` counts it:

```
precise: 631 calls asked (0 answered from cache): 244 guesses and 9 nothings made exact, 177 guesses
were library calls, 86 nothings confirmed, 115 unsettled  (10347 ms; pyright-langserver: 613 asked, ...)
```

Servers, used when installed: pyright (`.py`), rust-analyzer (`.rs`), gopls
(`.go`, also found in `$GOPATH/bin`) and TypeScript's tsserver (`.ts .tsx .js
.jsx .mts .cts .mjs .cjs`, its own protocol). Any other LSP server over stdio
works per extension:

```toml
[precise]
enabled = true                                   # default: only with --precise
servers = { ".java" = ["jdtls"], ".js" = [] }    # add or replace one; [] keeps a language static
wait = 300                                       # s a server may take to load the workspace
timeout = 60                                     # s without an answer before giving up on it
```

The built-in servers never fetch or build: gopls runs with `GOPROXY=off`,
rust-analyzer offline without build scripts or proc macros, and when cargo
can't resolve the workspace offline (dependencies never fetched) seamcheck
describes the workspace's own crates to it from `cargo metadata --no-deps`, so
calls between them and into std still resolve (std needs rustup's `rust-src`).
A server that is missing, crashes or stops answering is one warning on stderr;
the run goes on with the static graph.

Speed: requests are pipelined (256 in flight per server), servers run side by
side, and rust-analyzer is asked again until its workspace has loaded. Answers
are cached by the asking file's content, line, column and server, so a warm run
asks nothing, and an edit re-asks only the edited files.

Measured (`stats`, project calls exact; 4 cores, October 2026, no project
dependencies installed: no venv, no `node_modules`, no fetched crates):

| repo | language | static | precise | static run | precise cold |
|---|---|---|---|---|---|
| pallets/flask | Python | 90% | 96% | 1.1 s | 9.0 s |
| encode/httpx | Python | 92% | 98% | 1.2 s | 9.3 s |
| psf/requests | Python | 91% | 97% | 0.8 s | 6.3 s |
| honojs/hono | TypeScript (.ts / .tsx) | 92% / 83% | 95% / 98% | 3.8 s | 21.9 s |
| excalidraw/excalidraw | TypeScript (.tsx / .ts) | 91% / 89% | 95% / 96% | 8.1 s | 41.0 s |
| BurntSushi/ripgrep | Rust | 91% | 98% | 1.9 s | 19.5 s |
| sharkdp/bat | Rust | 91% | 98% | 2.4 s | 44.0 s |
| spf13/cobra | Go | 91% | 100% | 1.4 s | 2.1 s |
| junegunn/fzf | Go | 94% | 99% | 2.5 s | 7.7 s |

An unchanged repo loads its cached graph (0.2 to 1.0 s). A cold run is mostly
the servers' own analysis: pyright answers ~75 questions a second once warm,
tsserver ~200, and bat's 44 s is pyright on one 8,500-line numpy benchmark
fixture (its Rust took 14 s). Splitting a server's files over several processes
was slower (each re-analyses the same imports). What stays unsettled is mostly
callbacks held in parameters, interface signatures, and calls into dependencies
that aren't installed.

## Config: the project's files, never the tool's

seamcheck holds no project's rules. It reads the codebase's own files, merged
in this order:

1. `seamcheck.toml` at the project root
2. every `seamcheck.d/*.toml` beside it (split rules by area)
3. each `--config PATH` (a file, or a folder of .toml): rules kept outside a repo you don't own

`[project]` and `[runtime]` keys merge (include / exclude / packs lists add
up); `[[rule]]`, `[[bridge]]`, `[[invariant]]` and `[[contract]]` lists
concatenate. An id defined twice is an error, and findings name the file a
rule came from. `[project] packs` adds seamcheck's framework rules (below).

Not read by default: dependency and build folders (`node_modules`, `deps`,
`_build`, `target`, `dist`, `build`, `.venv`...) and third-party code kept in
the tree (`vendor/`, `third_party/`, `*.min.js`). Naming one in `include`
(`include = ["src/**", "vendor/mylib/**"]`) has it read.

Suppress one line: `# seamcheck: allow rule-id` (or `// seamcheck: allow
rule-id`) on the line or the line above. Every rule takes `severity`
(`error`, the default, or `warn`), `why` and `message`.

## Rules

```toml
[project]
name = "My project"
include = ["lib/**", "src/**/*.ts"]
exclude = ["**/generated/**"]          # node_modules, deps, _build, target... are always skipped

[[rule]]
id = "damage-pipeline"                 # forbid: a pattern may appear only where allowed
kind = "forbid"
pattern = '\.hp\s*-='
in = ["game/**"]
allow = ["game/entities/Unit.gd:take_hit"]   # path glob[:function glob]
why = "damage outside take_hit skips armor, shields, crits"

[[rule]]
id = "attack-passives"                 # together: where `if` matches, each `then` must too
kind = "together"
if = 'fire_bolt\('
then = ['_echo_shots\(']
scope = "near"                         # func (default) | file | near (window = [before, after] lines)
window = [0, 5]

[[rule]]
id = "handlers-exist"                  # coverage: ids defined in one place vs used in another
kind = "coverage"
defined = { in = ["data/*.ex"], pattern = 'id: :(\w+)' }
used = { in = ["lib/**"], pattern = ':(\w+)' }
check = ["unused", "undefined"]

[[rule]]
id = "writes-go-through-repo"          # reach: selected functions must reach a target within N calls
kind = "reach"
from = ["lib/my_app_web/controllers/**:*"]
when = 'Repo\.'                        # only functions whose code matches
to = ["authorize!"]
depth = 3

[[rule]]
id = "inject-before-enqueue"           # require: files must exist and contain the patterns
kind = "require"
in = ["lib/app/session.ex"]            # a glob must match at least one file
func = "start*"                        # optional: in each matching function
pattern = ['maybe_inject\(', 'enqueue\(']
ordered = true                         # optional: in this order
```

A `together` rule also takes `func` (only functions with these names) and
`depth = N` (a `then` may match in what the function calls, N exact calls deep:
an error path whose helper logs).

A `require` rule replaces a test that reads a source file and greps it: a
deleted file or renamed function is a finding, where a `together` rule would
pass silently. `forbid` takes `scope = "file"` for a pattern spanning lines,
and `repo = true` to read files outside `[project] include`.

`text = "code" | "nocomment" | "raw"` picks what patterns see: code (comments
and string contents blanked, the default for forbid and together), nocomment
(strings kept, the default for coverage), or raw. A rule whose regex doesn't
compile is a finding naming the rule; the other rules still run.

**A project's own check scripts** run as `command` rules, so one `check` and
one gate cover them:

```toml
[[rule]]
id = "design-kit"
kind = "command"
run = "node design/check_kit.mjs"     # exit 0 is clean; each output line is a finding
when = ["web/src/**", "design/**"]     # in a gate, run only when the change touches these
files = '^(\S+\.tsx?):'                # find the file each line is about
fix = "use the kit component"          # shown with each finding
gate = "touched"                       # block only on files the change touches
tighten = "node design/check_kit.mjs --write-allow"   # `seamcheck tighten`: shrink the allowlist
```

A command that can't run in this checkout (exit 127, a missing module) is one
warning that never blocks, not its stack trace as a dozen findings.

**How the gate treats a rule** (`gate`): `"all"` (the default: its errors
anywhere block), `"changed"` (its findings, warnings too, block only on the
lines a change adds or edits: old debt stays visible in `check`, new debt
can't land), `"touched"` (any finding in a file the change touches), `"off"`.

## One output, many views

A `fanout` rule checks one producer against every consumer: panel types the
hub emits against each client's renderer, events a channel pushes against each
client's listeners, paths clients call against the router.

```toml
[[rule]]
id = "panel-types"
kind = "fanout"
check = ["missing", "extra"]           # extra: a view handles what nothing produces
source = { in = ["lib/app/widget_type.ex"], within = '(?s)@known ~w\((.*?)\)', pattern = '(\w+)', text = "raw" }

[rule.views.phone]
in = ["android/**/Cards.kt"]
within = '(?s)fun from\(.*?\n    \}'    # only inside this region
pattern = '"(\w+)"'
normalize = [['_embed$', '']]           # what the view does to a token before looking it up
fallback = "Raw card"                    # unhandled tokens degrade here: a warning, not an error
except = { stage_labs = "desk-only" }    # intentional gaps, with the reason

[rule.views.desk]
in = ["native/src/Board.cpp"]
pattern = 'contains\("(\w+)"\)'
match = "contains"                      # exact | contains | prefix | regex | segments

[rule.views.hub]                          # a view (or source) can be a command's output
run = "mix phx.routes"
pattern = '(?m)^\s*\w+\s+(/api/\S*)'
rewrite = [[':\w+', ':p']]               # applied to each token as it is harvested
match = "segments"                      # `/a/:p/b` meets `/a/:id/b`; a trailing `**` is any rest
```

A view's `unhandled` (a regex) catches a branch that names a token and still
sends it to the default: with `unhandled = 'kind:\s*"center"'`, `case
"share_link": return { kind: "center" }` counts as missing. A branch body is
the text from the token to the next one (a fallthrough `case "a": case "b":`
shares the next body), cut at `default:` / `else ->`.

Every extractor (a source or a view) takes `in` / `skip` globs, `func` (only
inside these functions), `within` (only inside a regex's match), `pattern`
(group 1, or the match; a list is several), `values` (literal tokens), `text`,
`run` and `rewrite`. A `run` takes `inputs` (globs of what its output depends
on, like the router for `mix phx.routes`): the output is cached until one of
those files changes. `seamcheck matrix` prints the grid: `+` handled, `~`
except, `generic` fallback, `MISSING`, `extra`.

## Lints and essential controls

Some bugs are a shape, not a string: a row disabled by its data, a popup inside
a box that clips it, a table owned by a request process. `kind = "lint"` runs
one of seamcheck's structural checks over the files a project picks: seamcheck
knows what the shape means, the project says where it applies.

```toml
[[rule]]
id = "rows-open-whatever-their-state"
kind = "lint"
lint = "disabled-by-data"
in = ["android/**/*.kt", "web/src/**/*.tsx"]       # default: the check's languages
skip = ["**/*.test.tsx"]
options = { status = 'unread|read|pending' }       # each check's knobs (unknown ones are errors)
```

**UI**

| lint | finds | options |
|---|---|---|
| `disabled-by-data` | a tappable row/item/card (Kotlin `VRow(state = ..., onClick)`, JSX `disabled={...}` on a row, QML `enabled:` in a delegate) whose enabled state reads a data status (`notice.unread`, `row.pending`, `x.status`) and nothing about work in flight: its taps are dead | `status`, `busy`, `targets` |
| `inert-control` | a JSX `<button>` with no handler (not submit, not disabled), an uncontrolled `defaultValue`/`defaultChecked` input nothing reads, an empty `onClick = {}` / `onClicked: {}` (a tap-eater with `indication = null` or a disabled item is fine) | |
| `css-popup-clipped` | a JSX element whose CSS places it outside its containing block (`position: absolute; bottom: calc(100% + 4px)`) under an ancestor, at or above that containing block, that clips (`overflow: hidden/auto/scroll/clip`, `contain: paint`) | `css` (stylesheet globs) |
| `css-overlay-order` | a full-cover overlay (`inset: 0`, named like overlay/modal/dialog/scrim) whose z-index is below a shell layer's: it opens behind the dock | `css`, `above` (the ladder), `overlays`, `shell`, `min_shell_z` |
| `css-braces` | a stylesheet with an unclosed `{`: the build nests every later rule inside it | |

**QML**

| lint | finds | options |
|---|---|---|
| `qml-unknown-type` | a QML object type (`Sheet { }`) that no Qt module, project `.qml` file, inline `component` or C++ `QML_ELEMENT` / `QML_NAMED_ELEMENT` / `qmlRegister*` class defines: the page fails to load. A file importing a module seamcheck can't see into (MapLibre, Kirigami) is skipped | `types` (more known names) |
| `qml-singleton-import` | a file naming a singleton of its own QML module (`QT_QML_SINGLETON_TYPE`, a qmldir `singleton`, `pragma Singleton`) without importing that module: which object it gets depends on the Qt build (6.11.2 read the plain `Tokens.qml` beside it, an empty object) | |
| `qml-js-shadows-type` | a `.js` library named like a `.qml` type in the same module: the module lists both under one name and the library can hide the type | |
| `qml-anchors-in-positioner` | an item anchored inside a Column/Row/Grid/Flow (or a Repeater delegate in one) or a Qt Quick Layout: Qt refuses the anchor and stops laying out | |

QML modules come from CMake `qt_add_qml_module(... URI X ... QML_FILES ...)`
and `qmldir` files; Qt's own types are listed from Qt 6.11's type
descriptions (`seamcheck/qt_types.txt`).

**Kotlin, Elixir and ids**

| lint | finds | options |
|---|---|---|
| `kotlin-init-order` | an `init` block that uses, itself or through the class's own functions (`depth`, default 2), a property declared below it: Kotlin initialises in source order | `depth` |
| `id-kind-mix` | a `?:` / `??` / `\|\|` chain where a named id falls back to another object's bare `.id` (`notice.requestId ?: dest.id`); `named = true` also flags two differently named ids (`challenge_id \|\| request_id`) | `named` |
| `ets-owner` | `:ets.new` in a function no process callback (`init`, `handle_*` of a GenServer/Supervisor/Application module) reaches: the table dies with whichever request made it; `:heir` is fine | `owners`, `depth`, `tests` |
| `handle-info-catch-all` | a process that subscribes itself (in `init`, a callback, a private helper) and whose `handle_info` clauses all match specific messages: the first stranger on a shared topic crashes it | `subscribe` |
| `elixir-rescue-debug` | a `rescue` / `catch` whose only trace is `Logger.debug`: production drops debug, so the failure is invisible | |
| `auth-in-background` | a timer / `handle_info` / `handle_continue` that reaches a login or reconnect function: an unprompted auth challenge | `auth`, `roots`, `depth` |
| `home-paths-mounted` | an Elixir path under `$HOME` that no volume of the container service covers: it is lost on recreate | `compose`, `service`, `home` (default: the home its volumes mount into), `allow` |
| `stale-module-ref` | a doc or comment naming one of the project's own modules that no longer exists | `namespaces` |

**Scripts, services and builds**

| lint | finds | options |
|---|---|---|
| `shell-pkill-self` | `pkill -f` / `pgrep -f` inside an `ssh host "..."` or `bash -c "..."` string that holds the pattern, or naming the script's own file: it matches the shell running it, which dies before the rest runs (`'[m]ix phx.server'` can't match itself) | |
| `shell-reexec-watch` | a script that re-execs itself when its inputs change but doesn't watch every file it sources | |
| `shell-name-domain` | a host name used as a shell variable name or filtered through a class that drops `-` (`pylon-primos` became `pylonprimos`) | `names` |
| `path-unit-oneshot` | a systemd `.path` unit triggering a `Type=oneshot` service: changes that land while it runs are dropped | `settles` |
| `lockfile-sync` | `package.json` and `package-lock.json` disagree on a declared range (`npm ci` refuses) | |
| `toolchain-pin` | `rustup run X` / `cargo +X` naming a channel other than the one `rust-toolchain.toml` pins | |
| `test-reads-home` | a test reading the home directory: it passes only on the machine that has those files | `pattern` |

CSS is read just far enough: rules whose last compound is `.cls` (alone or in
context) apply to the class, `:hover`/`::before` and `.cls.is-x` variants
don't, the last declaration wins (`!important` beats it), and `var(--x)`
resolves from `:root`. JSX classes come from `className` strings and template
parts.

**Essential controls.** A bar's controls that must be reachable in every state
get a manifest. Each must exist, sit inside the bar, and have no conditional
(`{cond && ...}`, `cond ? ... :`, `if (...) {`, `visible: ...`) or menu
(`menus`, default: Menu/Popover/Dropdown/Dialog/... tags, headers and class
names) between it and the bar. JSX, Kotlin/Compose and QML.

```toml
[[rule]]
id = "chat-bar-essentials"
kind = "essential"
in = ["web/src/components/CommandInput.tsx"]
bar = 'className="command-row"'       # inside the bar, or its header: 'fun Composer\('
[rule.controls]
send = 'data-testid="send"'
auto_approve = { marker = 'data-testid="auto-approve"', disabled = false }   # may never be disabled
abort = { marker = 'data-testid="abort"', when = '^busy\b' }                # the one condition it may have
```

## Framework packs

Some rules are true for every project on a framework, not one project's
contract. seamcheck ships them as packs; a project turns them on:

```toml
[project]
packs = ["phoenix", "react"]

[[rule]]                         # override a pack rule by its id: its keys replace the pack's
id = "phoenix/no-atoms-from-strings"
severity = "error"
```

| pack | rules |
|---|---|
| `phoenix` | `ets-owner`, `handle-info-catch-all`, `rescue-logged-at-debug` (the lints above); `no-atoms-from-strings` (warn: atoms are never collected) |
| `react` | `inert-control`, `disabled-by-data` (JSX), `popup-clipped`; `effect-not-async` (an async effect's cleanup never runs), `state-not-assigned` (`this.state.x =` outside the constructor) |
| `qml` | `unknown-type`, `singleton-import`, `js-shadows-type`, `anchors-in-positioner` |
| `compose` | `init-order` (`kotlin-init-order`), `inert-control` (Kotlin); `no-global-scope` (warn: a `GlobalScope` coroutine outlives its screen) |
| `express` | (type knowledge only, below) |
| `hono` | (type knowledge only, below) |

A pack also says what its framework hands the code, so the call graph can type
it. That knowledge applies whenever package.json shows the framework, listed
in `packs` or not (a pack's rules still need listing):

```toml
[[param]]                          # packs/express.toml
id = "handler"
names = ["req", "res"]             # a function or callback taking (req, res...), (err, req, res...)
types = { req = "express:lib/request", res = "express:lib/response" }

[[param]]                          # packs/hono.toml
id = "route"
names = ["c"]
in_call = ["get", "post", "use", "on"]   # only a callback passed to app.get(...) and the like
types = { c = "hono:src/context" }

[[returns]]                        # packs/hono.toml: the RPC client is a runtime Proxy
id = "rpc-client"
functions = ["hc", "testClient"]
```

A type names a module of the framework's package: in an app, a call on `res`
is the library's (no guess at the app's own `send()`); in that package's own
repo, it is the module itself (`res.send()` in express's tests reaches
`lib/response.js`).

Pack rules are named `<pack>/<id>` (`seamcheck check --skip react/popup-clipped`
leaves one out of a run). An unknown pack, or an override naming a rule its
pack doesn't have, is a config error. `seamcheck init` lists the packs whose
framework a manifest declares: `:phoenix` in a mix.exs, `react` in a
package.json (`express`, `hono` likewise), Qt Quick in a CMakeLists.txt beside
`.qml` files, Compose in a Gradle build. A pack holds only what is true everywhere; a project's taste
stays in its own rules.

## One graph across languages: bridges and impact

Languages meet through strings (a URL, an event name, an op), so no parser
links them. A `[[bridge]]` says how: every `from` occurrence links its
enclosing function to what the matching `to` names. Then `callers`, `path`,
`unused` and `impact` see across the hub and its clients.

```toml
[project]
areas = { hub = ["lib/**"], phone = ["android/**"], web = ["web/**"] }   # how impact groups results

[[bridge]]                                   # a client's request -> the controller action
id = "http"
match = "segments"
from = { in = ["android/**", "web/src/**"], pattern = '"[^"]*?(/api/[^"]*)', rewrite = [['\$\{[^}]*\}|\$\w+', ':p']] }
to = { run = "mix phx.routes", pattern = '(?m)^\s*\w+\s+(/api/\S*)\s+(\S+)\s+:(\w+)', token = 1,
       rewrite = [[':\w+', ':p']], target = "{2}.{3}" }   # target: a function id from the groups

[[bridge]]                                   # a hub push -> each client's listener
id = "push"
from = { in = ["lib/**/channels/**"], pattern = 'push\(socket,\s*"([\w.]+)"' }
to = { in = ["web/src/**"], pattern = '\.on\(\s*"([\w.]+)"' }   # no target: the enclosing function
```

```
seamcheck impact lib/app/dives/finance.ex     what a change could break, grouped by area
seamcheck impact 'Inbox.list' --fuzzy         also follow name-guess edges (noisier)
seamcheck impact --since HEAD                  after an edit: what the functions your diff touched reach
seamcheck callers AppWeb.InboxController.index   -> the phone / web code calling that route [via http]
```

`impact` follows callers (who depends on it) plus bridges forward (who
receives what it emits), certain edges only. It stops where data moves through
app state rather than calls (a fetched JSON rendered later by a screen):
contract tests (golden payloads the clients parse) cover that part.

## An MCP server for agents

`seamcheck serve` speaks MCP (2025-06-18, JSON-RPC over stdio, no
dependencies) and keeps the loaded graph between calls, so an agent's loop of
small questions skips the load each one paid. Before each call it stats the
project's files and config (mtime + size) and reloads only when one changed,
appeared or went: an edit between two calls is seen by the second.

```sh
claude mcp add seamcheck -- seamcheck serve -C /path/to/repo
```

| tool | arguments | returns (JSON text) |
|---|---|---|
| `check` | `only`, `skip` (rule ids), `errors_only` | the findings, as `check --json` |
| `gate` | `since` (default `HEAD`), `staged` | `{blocked, changed_files, findings}`, as `gate --json` |
| `callers` / `callees` | `name`, `depth` (2), `fuzzy` | per match: exact and guess counts, what it reaches with depth |
| `path` | `from`, `to`, `fuzzy` | the chain, or `through_guesses_only` |
| `find` | `name` | id, file, lines, callers exact / guessed |
| `refs` | `name` | defined, callers, mentions |
| `impact` | `targets` (specs or files), `since`, `staged`, `depth` (4), `fuzzy` | reached functions by area |
| `stats` | | sizes, timing, per language how calls resolve |
| `unused` / `tests` | `since` | the rows `--json` prints |

A query that finds nothing is an error result saying why (`no function
matches 'x'`), not an empty answer.

## Keeping a big repo tidy

```
seamcheck tidy [--since REF]      one compact report: biggest files, duplication, dead code, test value
seamcheck unused                  functions nothing calls AND no other file names (scripts, config, scenes)
seamcheck test-only               product functions only tests call: dead code its tests keep alive
seamcheck tests                   tests reaching no product code; tests that read product source (grep tests)
seamcheck dups                    near-duplicate functions across files, grouped by file pair
seamcheck split FILE              a big file's features + call clusters ("knotted": split by feature)
seamcheck hotspots                biggest files, and how many files call into each
seamcheck refs NAME...            defined / called / mentioned anywhere (replaces grep loops)
seamcheck remove ID... [--apply]  delete functions or test blocks with their docs (dry run by default)
  --ids    print only ids:  seamcheck test-only --ids | xargs seamcheck remove --apply
  --since  only files changed since a git ref: check your own change before landing
```

"Unused" is conservative on purpose: a name that appears in any other file (a
router, a template, `mix run -e`, a .tscn signal, a manifest) is kept, as is
anything a framework calls (callbacks, `@impl`, `@Override`, `on*`,
`handle_*`...).

A test **reaches product code** through its calls, and also through what the
call graph can't follow:

- a repo script it runs (`System.cmd`, `Port.open`, `subprocess`, `spawn`,
  `ProcessBuilder`), named from the root (`"scripts/x.py"`) or from the test's
  own folder (`Path.expand("../../scripts/x.py", __DIR__)`);
- a Phoenix route it requests (`get(conn, ~p"/api/car")`,
  `conn |> post("/api/car/#{id}/fill", body)`): the router's scopes, aliases
  and `resources` lead to the controller action, and on from there;
- a repo file no adapter parses that it reads (a compose file, a firewall
  table, a unit): there, the file is the product.

**Copies** are near-duplicate functions across files (token shingles,
Jaccard). A function whose whole body is one literal table (a list of structs,
a map of labels, a tool schema) is never a copy: two tables share a shape by
design.

```toml
[tidy]
tests    = ["test/**", "**/*.test.ts"]                 # default covers the usual layouts
entry    = '^(changeset|render_\w+)$'                  # more framework entry names
entry_in = ["lib/app_web/controllers/**"]              # every function here is an entry point
corpus   = ["scripts/**", "config/**"]                 # where else names are looked for
suites   = { hub = ["test/**"], web = ["web/**"] }
domains  = { spotify = 'spotify|track', places = 'place|map|route' }   # for `split`
```

## The land gate

`seamcheck gate [--since REF]` answers "may this change land?" (exit 1 when
not). It reads what changed since `REF` (default `HEAD`: staged, unstaged and
untracked; `--staged`: only what is being committed), and fails on:

- any rule error, anywhere (the repo is kept at zero), with each rule's `gate`
  mode applied; the `deps` rules run only when a manifest changed, and a
  dependency finding blocks only in a manifest the change edited;
- new code naming a `[gate] forbid_added` word, as a whole part of an
  identifier on a changed line (`fallbackUrl`, `legacy_path`, `CompatLayer`,
  `compatibility`; not `shim` inside `StashImages`; comments and strings don't
  count);
- functions and tests the change declares (their `def` / `test` line is new or
  changed): unused and test-only functions, and tests that reach no product
  code or grep product source;
- functions whose lines it changes that copy a function elsewhere;
- a test or script that still names a file the change split up or moved.

It also warns, without blocking, when a language the change touches resolves
worse than at `REF`: its "project calls exact" share (`stats`) fell more than
2 points. That is a change that broke an import seamcheck followed, so its
calls fell back to name guesses and every rule over them sees less:

```
  warning (not blocking): TypeScript (.ts) calls into project code exact: 100.0% at HEAD, 0.0% now (3 of 3 -> 0 of 2)
```

`check`, `gate` and `stats` record the shares whenever the tree's code and
config are HEAD's (a small file in the cache, keyed by commit, seamcheck's
version and the config). With no record for `REF`, the gate measures that
commit's files itself when the project is small (300 parsed files, no bridge
that runs a command); otherwise it records and stays quiet until there is one.

Code the change only moved (a file split, a function moved between files) is
judged as old. Debt already in a file you touched doesn't block you; new debt
can't land. A `seamcheck: allow gate` comment on (or just above) a declaration
or line waives it; give the reason in the comment.

```sh
# .git/hooks/pre-commit
seamcheck gate --staged || { echo "fix the above, or waive a line with 'seamcheck: allow'"; exit 1; }
```

```toml
[gate]
forbid_added = ["fallback", "legacy", "compat", "shim"]
```

`gate --json` gives one flat list of findings, each with why and how to fix;
`--format sarif` and `--format github` give the same list to CI (below).
In a gate, per-file lints read only the touched files, command rules run only
when the change touches their `when` paths, and `run` outputs come from their
cache, so a gate after a small edit takes seconds.

### In CI

```yaml
# .github/workflows/seamcheck.yml
on: [pull_request]
jobs:
  seamcheck:
    runs-on: ubuntu-latest
    permissions: { contents: read, security-events: write }
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 0 }
      - uses: actions/setup-python@v5
        with: { python-version: "3.12" }
      - run: pipx install git+https://github.com/LordJulius/seamcheck
      # annotations on the diff, and a red check when the change may not land
      - run: seamcheck gate --since origin/${{ github.base_ref }} --format github
      # or: every rule as code-scanning alerts
      - run: seamcheck check --format sarif > seamcheck.sarif || true
      - uses: github/codeql-action/upload-sarif@v3
        with: { sarif_file: seamcheck.sarif }
```

## Dependencies

`seamcheck deps` reads every manifest under the root (mix.exs, package.json,
build.gradle[.kts] + gradle/libs.versions.toml, Cargo.toml, pyproject.toml,
requirements*.txt, go.mod) and looks for each dependency in the files that
manifest owns. What a dependency is called in code comes from the installed
package when it can be found: module names in mix `deps/<name>`, bins and
peerDependencies in `node_modules`, the packages inside the Gradle-cache jar /
aar, `[lib] name` of a crate, Python `top_level.txt`. Otherwise it's guessed
from the name, and the line says "(name guessed)": install deps for a sure
answer. In a git worktree, deps resolve from the main checkout's installed
packages.

```toml
[[rule]]
id = "lean-deps"
kind = "deps"
severity = "warn"
report = ["unused", "test-only", "dead-only"]   # the default
ignore = ["androidx.media3:media3-exoplayer-hls"]  # loaded by reflection
```

| status | meaning |
|---|---|
| unused | no reference in code, tests, config or scripts (notes who pulls it in anyway) |
| test-only | declared for runtime, only tests use it |
| dead-only | every code use sits in a function nothing calls: it goes with that code |
| peer | not referenced, but a declared package lists it optional / peer |
| plugin | not referenced, but it wires itself in (META-INF/services, components in its Android manifest) |
| config | only config, build files, scripts, CI, a mix task a script runs |
| used | code uses it (`--all` shows how many files; one file = removed with it) |

The first three are the actionable ones (`deps` exits 1 when any exist).
Libraries loaded by reflection (media3 HLS) look unused: `ignore` them in
`[deps] ignore = [...]` or the rule's `ignore`.

## Runtime checks

Static rules read the code; runtime rules judge what a running program says
about itself. The program (a test run, a bot, a fuzzer) prints JSON lines:

```
SEAMCHECK {"k": "state", "t": 12.5, "units": [...], "gold": 40, ...}    any fields: a snapshot
SEAMCHECK {"k": "act", "act": "build", "ok": true, "before": {...}, "after": {...}, "price": 30}
SEAMCHECK {"k": "end"}                                                  finished cleanly
SEAMCHECK {"k": "waive", "rules": ["tide-lives"], "why": "this test clears the map"}
```

A `waive` line switches rules off for the rest of the run, for a test that
breaks them on purpose. The summary always lists waived rules with their
reason, and waiving an id that doesn't exist is an error.

Other output is ignored, so the lines can share stdout with logs, except lines
matching `[runtime] fail_on` (the project's own crash markers, e.g.
`fail_on = ['SCRIPT ERROR', 'panicked at']`), which are findings. Writing the
emitter is the project's job, and it should only report, never judge: a few
lines in any language. The judging lives in the project's config:

```toml
[[invariant]]                  # every state line
id = "hp-in-range"
each = "units"                 # optional: per item (item fields become bare names)
where = "not dead"             # optional: skip when false
assert = "0 <= hp <= max_hp"
for = 2.0                      # optional: only once false this long (by t)
key = "id"                     # optional: matches an item across states (prev_item)
why = "..."

[[invariant]]
id = "frozen-while-paused"
when = "paused and prev and prev.paused"
each = "units"
assert = "prev_item is None or (x, z, hp) == (prev_item.x, prev_item.z, prev_item.hp)"

[[contract]]                   # every act line whose name matches
id = "refusal-changes-nothing"
act = ["*"]
when = "ok == False"
assert = "after == before"
```

Expressions are plain Python expressions over the line's fields, in a sandbox
(no calls but the helpers, no `_` attributes). Always in scope, winning over
fields of the same name: `line` (the whole line), `state` (the current state;
for a contract, the last one before it), `prev` (the previous state), `item`
and `prev_item`. `a.b` works on dicts, and a field beats a dict method
(`before.items` is the field). A field that may be missing: `get(line, "ok")`.
Helpers: `len set sorted min max sum abs any all round count(xs, v) dupes(xs)
isfinite(x) get(d, k, default) keys(d) values(d) diff(a, b)`.

A finding shows the assertion and the value of each name in it, e.g.
`0 <= hp <= max_hp  [hp=112, max_hp=100]`, once per rule and item, with a
repeat count.

**A run never passes by saying nothing.** It fails when no SEAMCHECK line
arrived (the emitter is off), when the stream ends without `end` (a crash or a
timeout; a child that exits ends the run even if a grandchild keeps the pipe
open), or when a rule can't be evaluated (a typo'd field is an error, not a
pass). `--strict` also flags rules nothing exercised. With no invariants or
contracts configured, `run` / `fuzz` exit 2.

**Fuzzing:** make the program's random choices come from one seed (read
`{seed}` from its arguments or `$SEAMCHECK_SEED`), then
`seamcheck fuzz --seeds 1-20 -- CMD`. Each failing seed prints the exact
command that replays its inputs. Logs go to `$TMPDIR/seamcheck-fuzz/`.

## Writing good contracts (for agents)

- A contract is a rule the code depends on that no single file shows: "every
  attack path fires the on-attack passives", "all damage goes through
  take_hit", "every event frees what it spawns", "every id in the data table
  has a handler". When you fix a bug caused by one system not knowing about
  another, add the contract so it can't come back.
- Prove a new rule works: re-introduce the bug in a scratch copy and check it
  fires. Tune `allow` so the current code is clean, with a comment on each
  deliberate exception.
- Prefer choke points (one function everything goes through) plus a forbid
  rule over patching many call sites.
- When a bug gets through anyway, write down which rule would have caught it.
  That list is what seamcheck grows from.

## Development

```sh
python3 -m unittest discover tests     # the suite; CI runs it on Python 3.11, 3.12 and 3.13
```

Each test reproduces the bug a check was written after, and its fix. A new
check needs both: the cut-down bug that must fail, and the fixed shape that
must pass.

The cache lives in `~/.cache/seamcheck/`. Parses are keyed by file content and
the linked graph by every file's content, both with a digest of seamcheck's own
source (and `CACHE_VERSION`, `seamcheck/core.py`), so every checkout of a repo
(a git worktree, a lane) shares them, an upgrade or a local change to an
adapter never reuses what the old code made, and the cache prunes itself to
3 GB.

`tests/test_languages.py` holds each language's resolution cases, cut down
from open-source repos (below); after changing an adapter, run `seamcheck
stats` on a real repo of that language and compare the "project calls exact"
share.

### Measured on open-source repos

How many of the calls that reach project code are exact (`seamcheck stats`),
before and after 0.3's adapter work, at each repo's HEAD in October 2026:

| repo | language | 0.2 | 0.3 | what changed |
|---|---|---|---|---|
| pallets/flask | Python | 59% | 58% | 1,244 -> 1,301 functions (same-named methods were lost); 0.2's share counted ~680 nested `def` lines as calls |
| encode/httpx | Python | 33% | 66% | imports, classes, `self`, constructors |
| spf13/cobra | Go | 36% | 89% | packages, go.mod imports, receivers |
| junegunn/fzf | Go | 47% | 75% | |
| BurntSushi/ripgrep | Rust | 49% | 76% | `use` across crates, std methods |
| sharkdp/bat | Rust | 44% | 66% | |
| excalidraw/excalidraw | TypeScript (.tsx) | 68% | 82% | class-field handlers, barrels, workspace packages |
| google/gson | Java | 80% | 89% | 5,300 -> 23,600 call sites (bodies ended at their first statement) |
| App-vNext/Polly | C# | (0 functions) | 31% | 4,811 functions (Allman braces) |
| plausible/analytics | Elixir + TS | 98% | 98% | (unchanged) |
| sinatra/sinatra | Ruby | (no adapter) | 93% | the Ruby adapter |
| jekyll/jekyll | Ruby | (no adapter) | 90% | |

## License

GPL-3.0-or-later (see `LICENSE`). Copyright (C) 2026 LordJulius.
