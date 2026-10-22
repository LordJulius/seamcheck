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
  GitHub annotations; exit code 1 on errors; a pre-commit gate; and queries (`callers`, `impact`, `refs`) that replace grep
  loops and say how sure they are.
- **Many languages, one graph.** Elixir, Kotlin, Java, TypeScript/JavaScript
  (JSX), C++, QML, Python, GDScript, Rust, Go and C#. Rules work on any text
  file.
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
the folders their code lives in, one commented example of each common rule
kind), and install the pre-commit gate:

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
  seamcheck init [--hook] [--force] a first seamcheck.toml for this repo (+ the pre-commit gate)

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
- a helper the calling function defines itself (`const nudge = () => ...`
  inside a component);
- a Kotlin extension the caller's own file declares (it shadows other files'
  of the same name), and overloads within the one module in scope;
- a method name that exists in exactly one module the file can see;
- a `[project.groups]` binding (below).

An edge is **fuzzy** (a name guess) when the call is on a receiver of unknown
type: every function with that name is a candidate, unless more than 8 share
it (then it's dropped as noise). Guesses are limited to what could be true: a
guess never crosses languages (Kotlin never calls TypeScript; QML may call its
JS libraries and C++), and product code is never guessed into a test file.

Some calls resolve to **nothing**, on purpose:

- a qualifier that names a module with no such function in the project
  (`Enum.reject`, `Repo.rollback`) is a library call;
- in Elixir, Kotlin, Java, TypeScript, Python, Go and Rust a bare call with
  nothing in scope is the language's own library (`Enum`, Compose's `Row`,
  vitest's `expect`, `len`, `append`, `println!`);
- a call on a name imported from outside the project (`json.dumps()`,
  `fmt.Println()`, `_.map()` after `import _ from "lodash"`);
- a call on a variable typed outside the project (`s: String; s.split()`),
  unless a project Kotlin extension is declared on that type;
- in Rust, `.unwrap()`, `.len()`, `.push()` and the other everyday
  Option / Result / Vec / String / iterator methods on a receiver of unknown
  type: they are the standard library's, not a project function of that name.

`seamcheck stats` prints, per language, how many call sites are exact, guessed
or nothing, and of the calls that reach project code, how many are exact.
Watch that number: a drop after a change means the tool lost an import.

Queries say how sure they are. `callers`, `callees`, `path` and `impact` walk
exact edges; `--fuzzy` adds the guesses. `find` and the `callers` header print
both counts (`callers: 12 exact, 1849 name-guess`).

Tests are functions too (kind `test`): ExUnit `test "..." do` (including a
head that goes on to the next line, like `test "x",\n %{conn: conn} do`),
vitest/jest `it(...)` / `test(...)`, JUnit `@Test`, pytest `test_*` functions
and `Test*` class methods in test files, Go `TestX` / `BenchmarkX` /
`ExampleX` / `FuzzX` in `_test.go`, Rust `#[test]` (and `#[tokio::test]`),
and xUnit / NUnit / MSTest `[Fact]` / `[Test]` / `[TestMethod]`. So `callees 'test *'`
says what a test exercises. ExUnit `setup` and `describe` are fixtures and
grouping, not tests.

Adapter details: a Java / C# method's parameters may span lines and its body
may open on the next line (Allman braces), a C# body may be `=> expr;`; a
TypeScript class field holding an arrow function (`private onDown = (e) =>
{`) is a function; Python module-level code (decorators, `app = create()`,
`if __name__ == "__main__":`) belongs to the file's `<top>`; C++ out-of-line methods, `->` calls and `25'000` digit
separators; QML functions, `onX:` handlers and component instances (a use of
`X.qml`); JSX `<Card />` is a call to `Card`; an Elixir sigil (`~w(a b)`) is
not a call. A file an adapter crashes on is a `config` error finding, never a
silently empty file.

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

## Config: the project's files, never the tool's

seamcheck holds no project's rules. It reads the codebase's own files, merged
in this order:

1. `seamcheck.toml` at the project root
2. every `seamcheck.d/*.toml` beside it (split rules by area)
3. each `--config PATH` (a file, or a folder of .toml): rules kept outside a repo you don't own

`[project]` and `[runtime]` keys merge (include / exclude lists add up);
`[[rule]]`, `[[bridge]]`, `[[invariant]]` and `[[contract]]` lists concatenate.
An id defined twice is an error, and findings name the file a rule came from.

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

C#'s share stays low: most of Polly's calls are fluent chains and extension
methods, which need type inference seamcheck doesn't do.

## License

GPL-3.0-or-later (see `LICENSE`). Copyright (C) 2026 LordJulius.
