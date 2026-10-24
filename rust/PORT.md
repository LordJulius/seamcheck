# seamcheck in Rust

The Python seamcheck (`seamcheck/`, 20.7k lines; `tests/`, 8.9k) gates every
commit in every repo that runs `seamcheck gate --staged`. This crate is its
replacement. The contract it keeps is the one users see: the CLI (commands,
flags, text and JSON output), the TOML config (`seamcheck.toml`,
`seamcheck.d/*.toml`, packs) and the behaviour the Python test suite pins
down. Nothing about the internals is kept for its own sake.

Decision O1 in elixir-pot's `docs/plans/language-condense.md`. Cutover (the
launcher at `~/.local/bin/seamcheck`, master, the public repo) happens only
once graph parity exists, and Julius decides it.

## Why: where the time goes today

Profiled on elixir-pot (4,135 files, Pylon Primos, 2026-10-23), one staged
line changed:

| run | wall |
|-----|------|
| `gate --staged`, cold cache | 109 s |
| `gate --staged`, warm cache | 14.6 s |
| `check`, warm | 9–13 s |

Warm gate, by phase (cProfile, 20.5 s under the profiler):

| phase | s | what |
|-------|---|------|
| `run_rules` | 11.5 | lints 7.5 (css-overlay-order 1.4, inert-control 1.3, toolchain-pin 1.3, css-popup-clipped 1.0, qml-unknown-type 0.5, handle-info-catch-all 0.4), fanout harvests 3.1, forbid / together / require 0.8, `match_any` over every file per rule 3.8 |
| `core.load` | 3.7 | unpickling the cached graph 2.4, listing files |
| `test_only` + `unused` + Corpus | 4.3 | `is_test` per function 2.6, the name corpus 1.1 |
| `dups`, `test_audit`, `exactness` | <1 | |

Cold (150 s under the profiler) the load is 127 s: `_build_edges` 68,
`_parse_parallel` 41, `_apply_bridges` 16; `run_rules` is 15. So the warm gate is mostly the rules, and the cold gate is
mostly parsing and linking: phase 1 takes the first, phase 2 the second.

## Module inventory

| Python module | lines | what it is | Rust home | phase |
|---------------|------:|------------|-----------|-------|
| `__main__.py` | 414 | argparse CLI, check / graph / query printing | `main.rs` | 1 (check, gate, matrix), 3 (the rest) |
| `config.py` | 171 | merge seamcheck.toml + seamcheck.d + --config, packs, key/severity/gate checks | `config.rs` | 1 |
| `scan.py` | 396 | blank comments and strings, keep layout; string literals | `scan.rs` | 1 |
| `core.py` (globs, walker, `_text_for`, rules) | ~900 of 3,692 | `glob_re`, `list_files` (rg), `_one_copy`, forbid / require / together / coverage / fanout / command / lint / essential, harvest, `run_rules` | `glob.rs`, `walk.rs`, `project.rs`, `rules/` | 1 |
| `core.py` (load, resolution, bridges, reach, dead) | ~2,800 | parse fan-out + caches, import resolution per language, `_build_edges` (550 lines), param flow, `[[bridge]]`, `reach` / `path` / `dead` | `graph/` | 2 |
| `model.py` | 106 | Module / Func / Call | `model.rs` | 2 (done) |
| `lang_elixir.py` | 237 | Elixir adapter | `lang/elixir.rs` | 2 (done) |
| `lang_other.py` | 3,012 | Python + brace languages (JS/TS/QML, Rust, Kotlin, Java, C#, Go, C/C++, Swift, PHP) | `lang/` | 2 |
| `lang_ruby.py` | 1,971 | Ruby adapter | `lang/ruby.rs` | 2 |
| `lang_gdscript.py` | 198 | GDScript adapter | `lang/gdscript.rs` | 2 |
| `chains.py`, `lambdas.py` | 782 | receiver chains, lambda parameter types | `lang/chains.rs`, `lang/lambdas.rs` (+ `_assignments`: `lang/assign.rs`) | 2 (done) |
| `infer.py` | 2,166 | receiver type inference over the graph | `graph/infer.rs` | 2 |
| `precise.py` | 848 | language servers settle guessed calls | `precise.rs` | 4 |
| `lint.py`, `lint_infra.py`, `lint_qml.py`, `structure.py`, `css.py` | 2,241 | `kind = "lint"` checks, `essential`, brace / JSX trees, CSS index | `lint/` | 1 (all but `auth-in-background` and `ets-owner`, which walk the graph: 2) |
| `tidy.py` | 980 | gate, dead / test-only / unused, dups, test audit, split, hotspots, remove, refs, moved code | `gate.rs` (1: changed lines, moved lines, forbid_added, moved refs) / `tidy/` | 1 / 3 |
| `tidy_cli.py` | 241 | tidy command output | `main.rs` | 1 (gate) / 3 |
| `exactness.py` | 176 | gate's import-resolution guard (warns) | `tidy/` | 3 |
| `deps.py` | 796 | declared vs used dependencies | `deps.rs` | 3 |
| `arch.py` | 737 | depend / layers / cycles / path rules, `graph` command | `arch.rs` | 3 |
| `queries.py` | 205 | answers as data (CLI and MCP share them) | `queries.rs` | 3 |
| `structure.py` | 316 | (see lint) | `lint/structure.rs` | 1 |
| `fingerprint.py` | 143 | content hashes, shared on-disk cache | `cache.rs` | 2 (only once parsing is worth caching; phase 1 needs no cache) |
| `serve.py` | 252 | MCP server over queries | `serve.rs` | 4 |
| `report.py` | 118 | HTML map | `report.rs` | 4 |
| `runtime.py`, `runcmd.py` | 559 | `trace` / `run` / `fuzz` (reads only config) | `runtime.rs` | 4 (independent, any time) |
| `init.py` | 203 | first seamcheck.toml, pack detection, hook | `init.rs` | 4 |
| `output.py` | 72 | SARIF, GitHub annotations | `output.rs` | 1 |

## What each command needs

| command | config | files + text | function spans (adapters) | call graph | types (infer) | other |
|---------|:-:|:-:|:-:|:-:|:-:|-------|
| `check` | x | x | x (func-scoped rules, `func` field) | reach / arch / path / together-with-depth | security pack | lints, command runner |
| `gate` | x | x | x | dead code, test-only, tests audit, dups (function bodies), exactness | | git diff |
| `matrix` | x | x | (harvest `func`) | | | |
| `callers`, `callees`, `path`, `find`, `context`, `search`, `impact`, `dead`, `stats`, `graph` | x | x | x | x | x | |
| `tidy`, `tests`, `test-only`, `unused`, `dups`, `split`, `hotspots`, `remove`, `refs` | x | x | x | x | | name corpus |
| `deps` | x | x | x | x (dead-only) | | manifests, installed packages |
| `map` | x | x | x | x | | HTML |
| `serve` | x | x | x | x | x | MCP |
| `trace`, `run`, `fuzz`, `tighten` | x | | | | | runtime rules / shell |
| `init` | | x | | | | pack detection |

## Phases

0. **Map** (this file).
1. **Config, walker, text, the rules without a graph, the gate's text half,
   the parity harness.** Done on branch `rust-port` (status below).
2. **Adapters and the call graph.** The model (`Module` / `Func` / `Call`),
   every adapter, import resolution, `_build_edges`, chains, lambdas, infer,
   bridges, `reach` / `path`. Exit: per-file parse dumps and the edge set
   match Python's on elixir-pot and the held-out repos (`bench/`), and the
   deferred rules run. The core (model, registry, graph, Elixir, the shared
   text helpers, the harness modes) is done; the adapter lanes are below.
3. **Graph consumers.** Gate's dead-code / test-only / test-audit / dups /
   exactness halves, tidy commands, deps, arch, queries. Exit: `gate`
   and `check` parity with nothing deferred; the Python suite's gate, tidy,
   deps and arch tests ported.
4. **The rest.** serve, map, precise, runtime (`trace` / `run` / `fuzz`),
   init, tighten. Exit: every command; the whole Python suite ported.
5. **Cutover**, when Julius says: the launcher runs the Rust binary, master
   and the public repo get the crate, the Python package goes. Then new work
   lands only in Rust: the Slint reader and the module-size rule (D5's
   split-at-~400-lines rule) are the first two.

## The parity harness

`cargo run --release --bin parity -- [--gate [--staged]] [--fixtures tests/fixtures] REPO...`

For each project it runs the Python seamcheck (`python3 -m seamcheck`, with
`PYTHONPATH` at this repo; `--python DIR` to point elsewhere) and this binary
side by side:

1. `seamcheck port-status --format json` (Rust) lists the rules phase 1
   defers, with the reason (a graph rule kind, a lint not ported, functions of
   a file whose adapter is phase 2).
2. `check --format json`: Python with `--skip <deferred>`, Rust as is.
3. With `--gate`: `gate --format json [--staged]` from both; Python's
   graph-only findings (`unused`, `test_only`, `dups`, `tests`,
   `exactness`) and those of deferred rules are set aside.
4. Findings compare as multisets of (kind, rule, severity, file, line,
   message, snippet), plus `func` for files whose functions phase 1 knows
   (Elixir, and files no adapter reads). `config` notes about `[[bridge]]`s
   are set aside (bridges are built with the graph). A bad regex is reported
   by both, each in its own engine's words; that text isn't compared.
5. It prints every difference and both wall times, and exits 1 on any.

`tests/fixtures/*` are small projects from the Python tests (each Rust test
that uses one says which Python test it ports); the harness runs both
implementations on them too.

Phase 2 modes (Python's side runs inline through `python3 -c`, no new
Python file): `--parse` compares each file's modules as its adapter gives
them, `--edges` the linked graph's edges from callers in those files
(bridges off, an edge into a language not ported yet listed apart), and
`--chains` the shared text helpers on every call site, assignment, declared
type and bracket of real files (Python answers the same sites on Rust's
blanked code). `--ext` picks the languages; `--check` adds the phase 1
comparison back.

## Known differences (by design or not worth matching)

- Error text for a bad regex or a TOML syntax error is each library's own.
- Offsets: Python's scan keeps characters in place; the Rust scan keeps bytes
  in place (a blanked `é` becomes two spaces). Rules see the same lines and
  the same matches; a pattern that counts characters inside blanked text
  (`\s{3}` across a blanked non-ASCII string) could differ.
- `view_tokens`' 400-character lookahead window is 400 bytes.
- Python's `$` (without `(?m)`) also matches before a final newline; rule
  texts are lines without one, so it never mattered.
- Python's `finditer` may return an empty match at the position of a
  previous empty match when a non-empty one also starts there; never in a
  rule pattern so far.

## Phase 1 status (2026-10-24)

**Commands.** `check` (text / json / sarif / github, `--only`, `--skip`,
`--errors-only`, `--config`), `gate [--staged | --base REF]` (changed lines,
moved lines, `forbid_added` names on changed lines, generated files,
version-only manifest edits, gate modes all / changed / touched / off, moved
references), `matrix [--all]`, `tighten`, and `port-status` (the deferred
list as data). Every other command exits 2 saying it is phase 2+.

**Rules.** Kinds `forbid`, `require`, `together` (no depth), `coverage`,
`fanout`, `command`, `lint`, `essential`. Every lint but `auth-in-background`
and `ets-owner`. On elixir-pot 115 of 129 rules run; 14 are deferred: graph
kinds (reach / depend / layers / cycles / path), `deps`, `together` with
depth, the two graph lints, and func-scoped rules over files whose adapter is
phase 2.

**Regex.** Python's `re` syntax is translated (`pyre.rs`); patterns run on
the `regex` crate, and only those with lookaround or backreferences on
`fancy-regex`, prefiltered by the same pattern with its lookarounds removed
so the backtracker starts only where a match can.

**Parity** (`parity --fixtures tests/fixtures`, `parity --gate --staged` on
elixir-pot and on `ep-gate`, a copy of it with a staged Python file, a shell
script, a fixtures read and `use_fallback` / `legacy_path` names): 0
differences. elixir-pot `check` 670 / 670 findings identical, its gate 0 / 0,
ep-gate's gate 6 / 6.

**Tests.** 80 Rust tests (the Python suite's config, scan, forbid, require,
together, coverage, fanout, command, gate-text and lint tests that need no
graph), each naming the Python test it ports.

**Wall time** on Pylon Primos, elixir-pot (4,135 files), warm Python cache:

| run | Python | Rust | load avg |
|-----|-------:|-----:|---------:|
| `check` | 16.5 s | 2.3 s | 6–8 |
| `gate --staged` | 11.3 s | 1.3 s | 6–8 |
| `check` | 25.5–28.7 s | 6.5–6.6 s | 18–20 |
| `gate --staged` | 18.2–19.9 s | 4.2–6.9 s | 18–20 |
| `check`, cold Python cache | 69–90 s | (no cache) | |

Rust has no cache to warm: what's left of its time is the `command` rules'
own processes (api-generated-models, the `*-calls-real-routes` checks), which
both pay alike. Rust's gate numbers leave out the graph half (dead code,
test-only, dups, test audit), which is about 4 s of Python's warm gate and
most of its cold one.

## Phase 2 status (core lane, 2026-10-24)

**In.** `model.rs` (model.py field for field), `lang::Adapter` and its
registry, the Elixir adapter (modules, functions, aliases, imports, `use`s,
calls, references), `graph/` (every ported file parsed in parallel, modules
indexed with `name@file` for a second one, calls linked in the Python
resolver's order: `graph/link.rs` keeps each branch where `_build_edges` has
it and marks the language- and type-specific ones `lane:`), and the text
helpers every other adapter shares: `lang/chains.rs` (receiver, expression,
type_name, elem_type, generic_name), `lang/lambdas.rs`, `lang/assign.rs`
(`_assignments`). `seamcheck port-dump parse|edges|chains` prints what the
harness compares.

**Parity.**

| what | where | result |
|------|-------|--------|
| `--parse` (every model field) | elixir-pot, 1,852 Elixir files | 0 differ |
| `--edges` | elixir-pot | 56,716 / 56,716 identical (56,652 exact) |
| `--parse` + `--edges` | ecto, hexpm, phoenix, plausible, oban | 0 differ (64,402 edges) |
| `--chains` (+ assignments) | elixir-pot (1,910 non-Elixir files, 929k sites) | 0 differ |
| `--chains` | SwiftyJSON, Dapper, urfave/cli, Slim, rack, fd | 0 differ (131k sites) |

Python's `\w` is emulated (`[\p{L}\p{N}_]`: `h₀` is a word, a combining
mark isn't); the parse parity found it.

**Time** (Pylon Primos, load 13-20): elixir-pot `--edges` Python 34.8 s
(warm parse cache, the whole project linked, bridges off) vs Rust 2.0 s
(everything from cold, Elixir only); `--parse` 9.3 s vs 1.8 s.

**Not yet:** `[[bridge]]` edges, supertypes / `children` / virtual dispatch
(the infer lane), parameter flow, the resolution counters (`stats`), and
every adapter but Elixir.

## Adapter lanes: the contract

Stable now: `model.rs` (every Python field is there; a lane that thinks it
needs another asks first), `lang::{Adapter, ParseCtx, ADAPTERS}`,
`graph::{Graph, parse_all, parse_file}`, `lang::{chains, lambdas, assign}`.

**What a lane implements.**

```rust
// src/lang/<name>.rs (or src/lang/<name>/mod.rs), plus one entry in lang::ADAPTERS
pub struct Kotlin;
impl Adapter for Kotlin {
    fn family(&self) -> &'static str { "kotlin" }           // its scan family
    fn exts(&self) -> &'static [&'static str] { &[".kt", ".kts"] }
    fn parse(&self, ctx: &ParseCtx, rel: &str, text: &str) -> Vec<Module>;
}
```

`parse` returns what the Python adapter returns, field for field
(`Module::dump` is the comparison). Rules of the road:

- Blank with `scan::blank(text, family, false)` (code + string literals).
  The Rust scan keeps bytes in place, so positions are byte offsets; pass
  them to `chains` / `lambdas` / `assign` as they are. Where Python counts
  characters into the output (`Call.col` is a character column, windows
  like `text[k:k + 300]`), count characters (`pyre::take_chars`).
- Every regex goes through `pyre::Rx` (Python's syntax and `\w`), never
  the `regex` crate directly. `Rx::match_at` searches forward from `pos`
  and keeps a match only if it starts there: fine for correctness, slow on
  a miss in a big file; add an anchored engine to pyre if a profile says so.
- Python's module flags `chains.CALL_TARGS` / `TRAILING_CLOSURES` are
  `chains::Opts::for_file(rel)`.
- Every Python dict that reaches the output is an `IndexMap` (insertion
  order); where Python's output order leaks from a `set`, say so in the
  lane's notes rather than guessing.
- A panic is caught and reported as Python's "parser error"; where the
  Python adapter raises on some input, panic with Python's message (see
  `lambdas::brace_lambda`).

**Tests.** The language's Python tests (test_kotlin.py, test_swift.py...),
parse-level asserts first, in `rust/tests/lang_<name>.rs`, with inline file
maps (as `tests/graph_elixir.rs` does) or `rust/tests/fixtures/lang/<name>/`.
Each test names the Python test it ports.

**Proof.** On Pylon-style setups (the Python repo at the lane's commit, the
bench clones as `owner__repo` in one directory):

```
cargo run --release --bin parity -- --parse --ext <exts> <elixir-pot copy> <bench repos...>
cargo run --release --bin parity -- --chains --ext <exts> <same repos>
```

Both 0 differences on elixir-pot (where the language is in it) and the
language's CORPUS and HOLDOUT repos (`bench/bench.py`). Then the same lane
takes its language's resolution: the `lane: <language>` branches in
`graph/link.rs`, its import form in `resolve_where` (`js:`, `py:`, `rs:`,
`go:`, `inc:`), and its project-wide passes (`lang_ruby.link`,
`qml_page_props`, `qt_dbus_interfaces`, `_python_names`, `_qml_context`);
proof `parity --edges --ext <exts>` 0 differences. Branches that need a
receiver's type wait for the infer lane.

**The lanes.**

| lane | Python | Rust home | exts | bench repos (CORPUS + HOLDOUT) |
|------|--------|-----------|------|------|
| A: brace | `lang_other.parse_brace` and everything it calls but the `_py_*` functions (~2,500 lines; `_parse_brace` alone is 905) | `lang/brace/` | .js .jsx .mjs .cjs .ts .tsx .mts .qml .rs .kt .kts .java .cs .go .c .cpp .cc .cxx .h .hpp .hh .swift .php | excalidraw, hono, plausible, express, hexpm, nowinandroid, okhttp, gson, jsoup, Polly, Newtonsoft.Json, cobra, fzf, ripgrep, bat, jq, redis, flameshot, kdeconnect, cool-retro-term, neochat, Alamofire, swift-argument-parser, Slim, guzzle; zod, ky, axios, koa, commons-lang, javapoet, moshi, Dapper, AutoMapper, gin, urfave/cli, clap, fd, lua, cJSON, fmt, spdlog, Kingfisher, SwiftyJSON, monolog, flysystem |
| B: Python + GDScript | `parse_python` and `_py_*` in lang_other; `lang_gdscript.py` | `lang/python.rs`, `lang/gdscript.rs` | .py .gd | flask, httpx, requests, click, rich; godot-demo-projects, tps-demo, Pixelorama |
| C: Ruby | `lang_ruby.py` | `lang/ruby.rs` | .rb .rake .ru | sinatra, jekyll, rack, sidekiq |
| D: types | `infer.py`, `core.resolve_bases`, the chain / typed-local / constructor / dispatch branches of `_build_edges`, `_param_flow` | `graph/infer.rs`, `graph/link.rs` | (all) | starts when lane A has JS/TS or Kotlin at parse parity |

Lane A is one function in Python (`_parse_brace` reads every brace language
through one walker and per-language tables), so it is one owner at first.
Once its walker, `_classes`, `_returns` and the JS/TS/QML parts are at
parity, a second lane can take the per-language import readers
(`_rs_imports`, `_kt_imports`, `_cs_imports`, `_swift_imports`,
`_php_imports`, `_go_imports`, `_rs_tree`) as `lang/brace/imports_<lang>.rs`.
Lanes B and C share nothing with A but the helpers already ported, so A, B
and C can start the same day on three machines; D starts later.

Phase 3 starts once the edge set matches for every language: gate's graph
half, tidy, deps, arch and queries split cleanly by command.
