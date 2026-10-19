# seamcheck

See a whole codebase at once and check the contracts between its parts.
Deterministic, local, no network, no dependencies beyond Python 3.11+ (and
ripgrep if present). Built for agents: plain-text or JSON output, exit code
1 on errors.

Languages: GDScript, Elixir, Python, JS/TS/TSX, Rust, Kotlin, Java, C#, Go,
C++ and QML. Rules work on any text file.

Every rule and lint was written after a real bug it would have caught, and
its test reproduces that bug (cut down) and its fix.

Speed: a Godot game (143 files, 2.4k functions) ~0.3-0.8 s; a Phoenix monorepo
with Android, Qt and React clients (4.1k files, 43k functions, 12 languages)
~1.5 s to load cached, ~6 s cold; its 117 rules run in ~23 s, most of it
the project's own command rules.
The parse cache lives in `~/.cache/seamcheck/` (per file, by mtime and size).

## Install

```sh
pipx install git+https://github.com/LordJulius/seamcheck   # or: pip install --user .
seamcheck check
```

Or run it from a checkout with no install: `PYTHONPATH=/path/to/seamcheck
python3 -m seamcheck check`. Tests: `python3 -m unittest discover tests`.

## Commands

```
seamcheck check                    run every contract in seamcheck.toml (exit 1 on errors)
seamcheck check --json             machine-readable findings
seamcheck check --only id,id       some rules
seamcheck callers NAME [--depth N] who calls it, transitively (overrides included)
seamcheck callees SPEC [--depth N] what it calls
seamcheck path FROM TO             the shortest call chain between two functions
seamcheck find NAME                where functions are defined (+ caller counts)
seamcheck dead                     functions nothing calls or references (a hint, not proof)
seamcheck stats                    files, functions, edges (exact vs fuzzy), timing, and
                                   per-language call sites resolved exact / guess / nothing
                                   (stdlib calls land in "nothing": watch it move, a drop
                                   after a change is the tool losing imports)
seamcheck map --html out.html      interactive map (humans; agents rarely need it)
seamcheck matrix [RULE] [--all]    a fanout rule's token x view grid (see "One output, many views")
seamcheck deps [--all] [--json]    declared dependencies vs use (see "Dependencies")
seamcheck deps NAME                every place one dependency is used

-C DIR          project root (default: nearest parent with seamcheck.toml)
--config PATH   more config: a .toml or a folder of them (repeatable)
--fuzzy         callers / callees / path / impact: also walk name-guess edges
                (exact edges are the default; `--exact` is still accepted)

Runtime (the program under test reports itself; see "Runtime checks" below):
seamcheck trace LOG...             check finished runs' output ('-' = stdin)
seamcheck run [--log F] [--timeout S] -- CMD...
                                   run a command, check what it reports while it runs
seamcheck fuzz --seeds 1-8 [--jobs 4] -- CMD... {seed} ...
                                   once per seed; lists failing seeds with the exact replay command
--strict        runtime: also flag rules that had nothing to check in this run
```

SPEC: `name` (glob ok), `Module.name`, or `path/glob:funcglob`.

Edges are **exact** when the target is certain (same module, parent class,
alias / preload / qualified module, a declared type) and **fuzzy** when the
call is on an unknown receiver: every function with that name is a
candidate, unless more than 8 share it (then it's dropped as noise).

Queries say how sure they are. `callers`, `callees`, `path` and `impact` walk
exact edges; `--fuzzy` adds the guesses. `find` and the `callers` header print
both counts (`callers: 12 exact, 1849 name-guess`), and when there are 0
exact callers the guess count gets its own line: "called from 0" and an
unlabeled 1861 are the same kind of wrong.

A qualifier that names a module (`Enum.reject`, `Repo.rollback`, `Vector3.UP`:
Capitalized, not an ALL_CAPS constant) and has no such function in the project
records nothing. It is a library call, or an alias that's wrong; never a guess
at every project function with that name (that gave one Kotlin `reject` 1861
callers and made `Map.get` an "exact" edge to Settings.get).

Declared types, no inference: inside a function, a variable the file declared
with a type resolves like an alias. Kotlin `val r: Repo` and parameters, TS
`const s: Store` and parameters, C++ `Type* p`, `Type& r`, `Type v(...)` and
by-value parameters. File-level too: Kotlin properties and constructor `val`s,
TS class fields, and a C++ class's header members for its `.cpp`
(`m_board->indexOf()`). A type outside the project resolves to nothing
(`s: String; s.split()` is not ours) unless a Kotlin extension is declared on
it (`fun JSONObject.optText`). GDScript locals typed `Node` stay duck-typed;
the Godot case is `[project.groups]` below.

Qualifiers in path-named languages (Kotlin, Java, TS, Rust, Python) resolve
by file name and by the types a file declares: `MapPrefs.set(...)` reaches the
function in `MapPrefs.kt` (or the file declaring `object MapPrefs`) exactly.
A call resolved only by its name is always fuzzy, even when one function has
that name, with one exception: a method call (`x.name()`, `a().name()`) whose
name exists in exactly one module the file can see (its imports, its package)
is exact.

Unqualified calls also resolve through imports: Elixir `alias A.{B, C}` (one
line or several) and `import A`; JS/TS `import {f as g} from "./x"`, `* as ns`,
default imports, and tsconfig `paths` (`@/lib/x`); Kotlin/Java same-package
files, `import a.b.fn`, `import a.b.*`. A bare call to a parameter or local
(`icon()` where `icon: () -> Unit`) never resolves by name. JSX `<Card />` is
a call to `Card`.

Tests are functions too (kind `test`): ExUnit `test "..." do` / `setup`,
vitest/jest `it(...)` / `test(...)`, JUnit `@Test`. So `callees 'test *'` says
what a test exercises, and a test reaching no product code is findable.

A file an adapter crashes on is a `config` error finding (it used to be
silently empty).

Tests: `python3 -m unittest discover tests`.

## Config: the project's files, never the tool's

seamcheck holds no project's rules. It plugs into a codebase and reads that
codebase's own files, merged in this order:

1. `seamcheck.toml` at the project root
2. every `seamcheck.d/*.toml` beside it (split rules by area: `static.toml`, `runtime.toml`...)
3. each `--config PATH` (a file, or a folder of .toml): rules kept outside a repo you don't own

`[project]` / `[runtime]` keys merge (include / exclude lists add up);
`[[rule]]`, `[[invariant]]`, `[[contract]]` lists concatenate. An id defined
twice is an error, and findings name the file a rule came from.

## seamcheck.toml

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
severity = "error"                     # error (fails) | warn

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
```

A `together` rule takes `func` (only functions with these names) and
`depth = N` (a `then` may match in what the function calls, N exact calls
deep: an error path whose helper logs):

```toml
[[rule]]
id = "channel-join-refusals-logged"
kind = "together"
in = ["lib/app_web/channels/**"]
func = "join"
if = '\{:error,(?![^\n]*(?:->|<-))'   # a returned error, not a matched one
then = 'Logger\.'
depth = 1
```

`text = "code" | "nocomment" | "raw"` picks what patterns see: code (comments
and string contents blanked, the default for forbid/together), nocomment
(strings kept, the default for coverage), or raw.

```toml
[[rule]]
id = "inject-before-enqueue"          # require: files must exist and contain the patterns
kind = "require"
in = ["lib/app/session.ex"]            # a glob must match at least one file
func = "start*"                        # optional: in each matching function
pattern = ['maybe_inject\(', 'enqueue\(']
ordered = true                         # optional: in this order
```

A `require` rule replaces a test that reads a source file and greps it: a
deleted file or renamed function is a finding (a `together` rule would pass
silently). `forbid` takes `scope = "file"` for a pattern spanning lines.
A rule whose regex doesn't compile is a finding naming the rule; the other
rules still run.

```toml
[[rule]]
id = "lean-deps"                       # deps: declared dependencies nothing uses
kind = "deps"
severity = "warn"
report = ["unused", "test-only", "dead-only"]   # the default
ignore = ["androidx.media3:media3-exoplayer-hls"]  # loaded by reflection
```

Suppress one line: `# seamcheck: allow rule-id` (or `// seamcheck: allow rule-id`)
on the line or the line above.

### `[project.groups]`: what a node found by group is

Godot code finds singletons by group, not by type:
`var main := get_tree().get_first_node_in_group("match")` then `main.hum_of(team)`.
`main` is no alias and no file stem, so those calls used to be name guesses, and
`hotspots` / `impact` (which count exact edges) saw the match root as unreachable.
The project states which script a group holds:

```toml
[project.groups]
match = "game/Main.gd"
events = "game/systems/Events.gd"
```

Inside one function, `var NAME := <x.>get_tree().get_first_node_in_group("match")`
binds `NAME` to that script for the rest of the function, and `NAME.foo()` is an
exact call, the same as a preload alias. A helper (`var main := _main()`) does not
bind; a group not in the table stays a name guess; a file that isn't in the
project is a config problem. A unique name is never made exact on its own: a
second `hum_of` would silently retarget every call. List only groups exactly
one script joins.

## One output, many views

A `fanout` rule checks one producer against every consumer: panel types the
hub emits vs each client's renderer, events a channel pushes vs each client's
listeners, paths clients call vs the router.

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

Every extractor (a source or a view) takes: `in` / `skip` globs, `func`
(only inside these functions), `within` (only inside a regex's match),
`pattern` (group 1, or the match; a list is several), `values` (literal
tokens), `text`, `run`, `rewrite`. `seamcheck matrix` prints the grid:
`+` handled, `~` except, `generic` fallback, `MISSING`, `extra`.

## Structure: lints and essential controls

Some bugs are a shape, not a string: a row disabled by its data, a popup inside
a box that clips it, a table owned by a request process. `kind = "lint"` runs
one of seamcheck's structural checks over the files a project picks; seamcheck
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

| lint | finds | options |
|---|---|---|
| `disabled-by-data` | a tappable row/item/card (Kotlin `VRow(state = ..., onClick)`, JSX `disabled={...}` on a row, QML `enabled:` in a delegate) whose enabled state reads a data status (`notice.unread`, `row.pending`, `x.status`) and nothing about work in flight: its taps are dead | `status`, `busy`, `targets` |
| `inert-control` | a JSX `<button>` with no handler (not submit, not disabled), an uncontrolled `defaultValue`/`defaultChecked` input nothing reads, an empty `onClick = {}` / `onClicked: {}` (a tap-eater with `indication = null` or a disabled item is fine) | |
| `qml-anchors-in-positioner` | an item anchored inside a Column/Row/Grid/Flow (or a Repeater delegate in one) or a Qt Quick Layout: Qt refuses the anchor and stops laying out | |
| `kotlin-init-order` | an `init` block that uses, itself or through the class's own functions (`depth`, default 2), a property declared below it: Kotlin initialises in source order | `depth` |
| `id-kind-mix` | a `?:` / `??` / `\|\|` chain where a named id falls back to another object's bare `.id` (`notice.requestId ?: dest.id`); `named = true` also flags two differently named ids (`challenge_id \|\| request_id`) | `named` |
| `ets-owner` | `:ets.new` in a function no process callback (`init`, `handle_*` of a GenServer/Supervisor/Application module) reaches: the table dies with whichever request made it; `:heir` is fine | `owners`, `depth`, `tests` |
| `handle-info-catch-all` | a process that subscribes itself (in `init`, a callback, a private helper) and whose `handle_info` clauses all match specific messages: the first stranger on a shared topic crashes it | `subscribe` |
| `css-popup-clipped` | a JSX element whose CSS places it outside its containing block (`position: absolute; bottom: calc(100% + 4px)`) under an ancestor, at or above that containing block, that clips (`overflow: hidden/auto/scroll/clip`, `contain: paint`) | `css` (stylesheet globs) |
| `css-overlay-order` | a full-cover overlay (`inset: 0`, name like overlay/modal/dialog/scrim) whose z-index is below a shell layer's: it opens behind the dock | `css`, `above` (the ladder), `overlays`, `shell`, `min_shell_z` |
| `qml-unknown-type` | a QML object type (`Sheet { }`) that no Qt module, project `.qml` file, inline `component` or C++ `QML_ELEMENT` / `QML_NAMED_ELEMENT` / `qmlRegister*` class defines: the page fails to load. Files importing a module seamcheck can't see into (MapLibre, Kirigami) are skipped | `types` (more known names) |
| `qml-singleton-import` | a file naming a singleton of its own QML module (`QT_QML_SINGLETON_TYPE`, qmldir `singleton`, `pragma Singleton`) without `import` of that module: which object it gets depends on the Qt build (6.11.2 read the plain `Tokens.qml` beside it, an empty object) | |
| `qml-js-shadows-type` | a `.js` library named like a `.qml` type in the same module: the module lists both under one name and the library can hide the type | |
| `elixir-rescue-debug` | a `rescue` / `catch` whose only trace is `Logger.debug`: production drops debug, so the failure is invisible | |
| `shell-pkill-self` | `pkill -f` / `pgrep -f` inside an `ssh host "..."` or `bash -c "..."` string that holds the pattern, or naming the script's own file: it matches the shell running it, which dies before the rest runs (`'[m]ix phx.server'` can't match itself) | |
| `shell-reexec-watch` | a script that re-execs itself when its inputs change but doesn't watch every file it sources | |
| `shell-name-domain` | a host name used as a shell variable name or filtered through a class that drops `-` (`pylon-primos` became `pylonprimos`) | `names` |
| `home-paths-mounted` | an Elixir path under `$HOME` that no volume of the container service covers: it is lost on recreate | `compose`, `service`, `home` (default: the home its volumes mount into), `allow` |
| `auth-in-background` | a timer / `handle_info` / `handle_continue` that reaches a login or reconnect function: an unprompted auth challenge | `auth`, `roots`, `depth` |
| `stale-module-ref` | a doc or comment naming one of the project's own modules that no longer exists | `namespaces` |
| `lockfile-sync` | `package.json` and `package-lock.json` disagree on a declared range (`npm ci` refuses) | |
| `path-unit-oneshot` | a systemd `.path` unit triggering a `Type=oneshot` service: changes that land while it runs are dropped | `settles` |
| `css-braces` | a stylesheet with an unclosed `{`: the build nests every later rule inside it | |
| `toolchain-pin` | `rustup run X` / `cargo +X` naming a channel other than the one `rust-toolchain.toml` pins | |
| `test-reads-home` | a test reading the home directory: it passes only on the machine that has those files | `pattern` |

CSS is read just far enough: rules whose last compound is `.cls` (alone or in
context) apply to the class, `:hover`/`::before` and `.cls.is-x` variants
don't, the last declaration wins (`!important` beats it), `var(--x)` resolves
from `:root`. JSX classes come from `className` string and template parts.

**Essential controls.** A bar's controls that must be reachable in every
state get a manifest. Each must exist, sit inside the bar, and have no
conditional (`{cond && ...}`, `cond ? ... :`, `if (...) {`, `visible: ...`)
or menu (`menus`, default: Menu/Popover/Dropdown/Dialog/... tags, headers and
class names) between it and the bar. JSX, Kotlin/Compose and QML.

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

**Gating heuristics.** A rule's `gate` says how `seamcheck gate` treats it:
`"all"` (default: its errors anywhere block), `"changed"` (its findings,
warnings too, block only on lines the change adds or edits: old debt stays
visible in `check`, new debt can't land), `"off"`.

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
seamcheck callers ElixirPotWeb.OsaController.inbox   -> the phone / web code calling that route [via http]
```

`impact` follows callers (who depends on it) plus bridges forward (who
receives what it emits), certain edges only. It stops where data moves
through app state rather than calls (a fetched JSON rendered later by a
screen): contract tests (golden payloads the clients parse) cover that part.

Adapters: GDScript, Elixir, Python, JS/TS (JSX), Rust, Kotlin, Java, C#, Go,
**C++** (out-of-line methods, `->` calls, `25'000` separators) and **QML**
(functions, `onX:` handlers, component instances as uses of `X.qml`).

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

"Unused" is conservative on purpose: a name that appears in any other file
(a router, a template, `mix run -e`, a .tscn signal, a manifest) is kept, as
is anything a framework calls (callbacks, `@impl`, `@Override`, `on*`,
`handle_*`...). The name index (every identifier in every file) is cached
per file mtime, so reruns cost about a second.

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

- any rule error, anywhere (the repo is kept at zero); the `deps` rules run
  only when a manifest changed; a rule with `gate = "changed"` blocks only on
  the lines the change adds or edits (warnings too),
- new code naming a `[gate] forbid_added` word (in identifiers on changed
  lines; comments and strings don't count; `seamcheck: allow gate` waives a line),
- functions and tests the change declares (their `def` / `test` line is new
  or changed): unused and test-only functions, tests that reach no product
  code or grep product source,
- functions whose lines it changes that copy a function elsewhere.

A test reaches product code through its calls, and also through what the
call graph can't follow:

- a repo script it runs (`System.cmd`, `Port.open`, `subprocess`, `spawn`,
  `ProcessBuilder`), named from the root (`"scripts/x.py"`) or from the
  test's own folder (`Path.expand("../../scripts/x.py", __DIR__)`),
- a Phoenix route it requests (`get(conn, ~p"/api/car")`,
  `conn |> post("/api/car/#{id}/fill", body)`): the router's scopes, aliases
  and `resources` lead to the controller action, and on from there,
- a repo file no adapter parses that it reads (a compose file, a firewall
  table, a unit): there the file is the product.

A function whose whole body is one literal table (a list of structs, a map of
labels, a tool schema) is never a copy: two tables share a shape by design.
A forbidden word must be a whole part of a name (`fallbackUrl`,
`legacy_path`, `CompatLayer`, `compatibility`), not letters inside another
word (`shim` in `StashImages`).

A `seamcheck: allow gate` comment on (or just above) the declaration waives it.

Debt that was already in a file you touched doesn't block you; new debt
can't land. Run it from a pre-commit hook:

```sh
seamcheck gate --staged || { echo "fix the above, or waive a line with 'seamcheck: allow'"; exit 1; }
```

```toml
[gate]
forbid_added = ["fallback", "legacy", "compat", "shim"]
```

Slow `run` commands (a route listing) take `inputs`: their output is cached on
disk until those files change, so a gate after a one-file edit takes seconds.

## Dependencies

`seamcheck deps` reads every manifest under the root (mix.exs, package.json,
build.gradle[.kts] + gradle/libs.versions.toml, Cargo.toml, pyproject.toml,
requirements*.txt, go.mod) and looks for each dependency in the files that
manifest owns. What a dependency is called in code comes from the installed
package when it can be found: module names in mix `deps/<name>`, bins and
peerDependencies in `node_modules`, the packages inside the Gradle-cache jar /
aar, `[lib] name` of a crate, Python `top_level.txt`. Otherwise it's guessed
from the name, and the line says "(name guessed)": install deps for a sure
answer.

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

## Writing good contracts (for agents)

- A contract is a rule the code depends on that no single file shows: "every
  attack path fires the on-attack passives", "all damage goes through
  take_hit", "every event frees what it spawns", "every id in the data table
  has a handler". When you fix a bug caused by one system not knowing about
  another, add the contract so it can't come back.
- Prove a new rule works: re-introduce the bug in a scratch copy and check it
  fires. Tune `allow` so the current code is clean, with a comment on each
  deliberate exception.
- Prefer choke points (one function everything goes through) + a forbid rule
  over patching many call sites.

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

Other output is ignored, so the lines can share stdout with logs, except
lines matching `[runtime] fail_on` (the project's own crash markers, e.g.
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
fields of the same name: `line` (the whole line), `state` (current state; for
a contract the last one before it), `prev` (previous state), `item`,
`prev_item`. `a.b` works on dicts, and a field beats a dict method
(`before.items` is the field). A field that may be missing: `get(line, "ok")`. Helpers: `len set sorted min max sum abs any all round count(xs, v)
dupes(xs) isfinite(x) get(d, k, default) keys(d) values(d) diff(a, b)`.

A finding shows the assertion and the value of each name in it, e.g.
`0 <= hp <= max_hp  [hp=112, max_hp=100]`, once per rule and item, with a
repeat count.

**A run never passes by saying nothing.** It fails when no SEAMCHECK line
arrived (the emitter is off), when the stream ends without `end` (a crash or
a timeout), or when a rule can't be evaluated (a typo'd field is an error,
not a pass). `--strict` also flags rules nothing exercised. With no
invariants or contracts configured, `run` / `fuzz` exit 2.

**Fuzzing:** make the program's random choices come from one seed (read
`{seed}` from its arguments or `$SEAMCHECK_SEED`), then
`seamcheck fuzz --seeds 1-20 -- CMD`. Each failing seed prints the exact
command that replays its inputs. Logs go to `$TMPDIR/seamcheck-fuzz/`.

Tests: `python3 -m unittest discover tests`.

## License

GPL-3.0-or-later (see `LICENSE`). Copyright (C) 2026 LordJulius.
