# `seamcheck verify`: drive the real app, judge the evidence

Lints read the code. `verify` runs the built app the way a person does, by
the stable ids of its controls, keeps what it saw, and judges it with the same
`[[invariant]]` / `[[contract]]` rules `seamcheck run` uses. A recipe that was
never run is a draft, and the gate says so.

The idea is pstack's per-app verification recipe (launch, doctor, drive,
evidence, cleanup, feature map, self-proof), made into config a tool runs
instead of prose an agent may skip. The code is Rust (`rust/src/verify/`,
`rust/src/runtime/`); the Python CLI hands `seamcheck verify ...` to
`rust/target/release/seamcheck` unchanged (`SEAMCHECK_RUST` overrides the path;
a missing binary exits 127, which a gate treats as "can't run").

## Files

```
seamcheck.d/verify/<target>.toml           how to reach, check, drive and leave the app
seamcheck.d/verify/<target>/<feature>.toml one user-facing feature, from the user's side
seamcheck.d/verify/<target>/proof.toml     written by --prove: which features ran clean, by hash
```

`seamcheck.d/*.toml` (the lint config) is not recursive, so nothing here leaks
into `check` or `gate`. Unknown keys are errors in every file.

### The target

```toml
[target]
driver = "adb"                 # adb is built; http, pty, cdp are seams (see Drivers)
why = "Mob, the Android client, on a test phone"
covers = ["android/veridian-mob/lib/veridian_mob/*_screen.ex"]   # a change here wants a feature
common = ["android/veridian-mob/lib/veridian_mob/shell.ex"]      # controls every feature may use (dock, bars)

[adb]
device = "$SEAMCHECK_ADB_SERIAL"   # required: verify never picks a device
deny = ["SM-F9*", "192.168.86.25:*"]   # globs over serial, hardware serial and model: never driven
package = "org.veridian.osa.mob"
activity = "org.veridian.osa.veridian_mob.MainActivity"
release_only = true            # doctor refuses a debuggable build
id_pattern = '^[a-z][a-z0-9_]*(\.[a-z0-9_<>-]+)+$'   # when a content-desc counts as an id (this is the default)

[launch]                       # reach a usable state from empty
start = true                   # am start the activity
reset = "veridianmob://home"   # a link back to the root, opened after start and at cleanup
ready = "nav.now"              # an id that must show
timeout = 30

[doctor]                       # read-only: is this instance worth driving?
checks = ["device", "awake", "unlocked", "package", "release", "foreground"]
probe = ["hub"]                # each named probe must answer

[[probe]]                      # read-only side effects: GET, JSON
id = "hub"
url = "http://localhost:4000/api/screen"
token = { env = "HUB_REMOTE_TOKEN", file = "~/.config/veridian/hub-remote.token" }   # sent on curl's stdin

[evidence]
dir = "..."                    # optional; see Evidence
logcat = ["Elixir:V", "AndroidRuntime:E"]   # logcat filter specs (everything else silenced)

[runtime]
fail_on = ["FATAL EXCEPTION", "Fatal signal \\d+"]   # a log line matching is a finding
```

### A feature

```toml
[feature]
id = "stash.lock"
title = "Stash opens behind its PIN pad, which takes every tap"
user = "What a person does and sees, in their words."
screens = ["android/veridian-mob/lib/veridian_mob/stash_screen.ex"]   # the code it covers
capture = "tree"               # full (default): screenshots, trees, log | tree: see Private screens
keep = ["stash.pin*", "stash.fingerprint*"]
steps = [
  { tap = "nav.dives" },
  { tap = "dives.card.stash" },
  { wait = "stash.pin_pad" },
  { tap = "stash.pin_pad", at = "top", expect = "unchanged", why = "the pad's background takes the tap" },
  { wait = "places.hidden.0", scroll = "places.body", timeout = 40 },
  { back = true },
]

[[contract]]                   # judged on each act line
id = "pad-tap-goes-nowhere"
act = ["tap:stash.pin_pad"]
assert = "'stash.pin_pad' in after.ids and after.els['stash.pin.entry'].text == before.els['stash.pin.entry'].text"

[[invariant]]                  # judged on each state line
id = "nothing-of-stash-before-unlock"
when = "'stash.pin_pad' in ids"
assert = "outside.ids == 0 and outside.texts == 0"
```

Steps: `tap`, `long_press`, `type` (with `text`, sent a key at a time), `swipe`
(with `dir`), `back`, `key` (a KEYCODE name), `open` (a URI), `wait`, `gone`,
`probe`. Options: `at` (where on the element: center, top, bottom, left, right,
or a corner like top-left, 12 px inside), `expect` (`changed`, the default for
an action; `unchanged`; `any`), `timeout` (seconds), `scroll` (a `wait` swipes
this container towards `dir` until the element shows), `why`. There is no x/y
step: a control is reached by its id or not at all.

## The evidence

Each run writes, into its own directory:

- `stream.jsonl`: the runtime protocol. A `state` line after launch and after
  every step (`focus`, `front`, `ids`, `els` {id: text, enabled, selected,
  checked}, `texts`, `probes`), an `act` line per step (`act` = `tap:<id>`,
  `id`, `ok`, `expect`, `changed`, `error`, `before`, `after`), and `end` after
  a clean cleanup.
- `NN-<step>.xml` (the tree) and `NN-<step>.png` (screenshot) per step.
- `logcat.txt` from launch on; `verdict.json`; `meta.json` (device, app version, feature hash).

Where: by default `$XDG_CACHE_HOME/seamcheck/verify/<repo>/<target>/<stamp>-<feature>/`,
outside the repository, because screenshots and trees hold whatever the app
showed. `[evidence] dir` may move it; a dir inside the work tree is refused
unless git ignores it. Verify never deletes evidence.

## Judging

The runtime checker, ported to Rust (`rust/src/runtime/`, pinned by
`tests/runtime.rs`, a port of `tests/test_runtime.py`), judges the stream. Verify's
own expectations are runtime rules added to every feature, so one checker
gives the verdict:

- `verify.step-ran`: every step ran (its control was on screen; the finding lists what was);
- `verify.expect`: a step's outcome matched its `expect` (a tap that should do
  something did, a tap an overlay should eat changed nothing);
- `verify.in-front`: the app stayed in front;
- plus `fail_on` log lines, and no `end` line when cleanup failed.

`seamcheck trace --config <feature>.toml <dir>/stream.jsonl` re-judges an old
run with the Python checker.

## Private screens (`capture = "tree"`)

No screenshot. The tree and the state keep only the subtrees under `keep`
(their ids and text) and the ids of their ancestors; every other node of the
app is only counted (`outside.ids`, `outside.texts`, and `outside.names` for
ids outside the private area's prefix). The log keeps only `fail_on` lines.
The driver still sees the whole screen in memory, to find controls and to tell
whether a step changed anything, but never writes it down. An invariant over
`outside` proves nothing else is on screen without recording what it would be.

## Drivers

A driver (`rust/src/verify/driver.rs`) is: `doctor`, `launch`, `reset`,
`observe`, `act`, `screenshot`, `log`, `cleanup`, `describe`. `adb` is built:

- **observe**: `uiautomator dump`. An element's id is its `resource-id` (an
  app view's `<pkg>:id/<name>` reads as `<name>`; framework ids are dropped),
  else its `content-desc` when that matches `id_pattern`. Mob's ids are Compose
  testTags made resource-ids (Veridian patch P24); its native views use
  `contentDescription`.
- **act**: a real touch (`input tap` / `swipe`) inside the element's bounds,
  once the element has held still across two dumps; an element less than 24 px
  on screen is refused (scroll it in first). So an overlay that should eat a
  tap really gets it.
- **guards**: before every action the device must be unlocked and the app in
  front; verify never touches a lock screen, a system dialog or another app.
- **cleanup**: an app verify started is force-stopped; one already running is
  left running, back at `reset`.

`http` (requests as acts, responses as states), `pty` (a terminal app, its
screen text as state) and `cdp` (a browser, the DOM by `data-testid`) fit the
same trait.

## Commands

```
seamcheck verify --list [TARGET]           targets, features, proof (proven / stale / draft)
seamcheck verify --doctor TARGET           the read-only checks only
seamcheck verify TARGET [FEATURE...]       doctor, launch, drive, evidence, cleanup, judge (exit 1 on errors)
seamcheck verify --prove TARGET [FEATURE...]
                                           the same, then re-read each run's evidence after cleanup and
                                           re-judge it from disk; clean -> proof.toml records the hash
seamcheck verify --check [--staged | --since REF]
                                           offline, for the gate (below)
```

## The gate

`--check` prints `path:line: message` findings and exits 1 when there are any:
a file that doesn't load; a step id none of the feature's `screens` (or the
target's `common`) files mention, literally or as a stem the code fills in
(`finance.tab.#{id}`, `dives.card.<id>`, `"#{prefix}.pin_pad"`); a feature
never proven (draft) or changed since its proof (stale); and with `--staged` /
`--since`, a changed file under `covers` no feature's `screens` names (proof
findings then only for features the change touches). A repo turns it on with a
command rule:

```toml
[[rule]]
id = "verify-map"
kind = "command"
run = "seamcheck verify --check --staged"
when = ["android/veridian-mob/lib/**", "seamcheck.d/verify/**"]
files = '^([\w./-]+):\d+:'
severity = "warn"              # first; error once the map is complete
gate = "touched"
```
