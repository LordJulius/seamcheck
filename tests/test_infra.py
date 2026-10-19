"""Lints across deploy scripts, containers and services (lint_infra.py).

Each case is a bug from the night of 2026-10-06 that every gate passed:
the fixture is the shape that shipped, and the fixed shape must stay clean.
"""
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core, lint_infra as L  # noqa: E402

AGENT_HEAD = '#!/usr/bin/env bash\nHERE="$(dirname "$0")"\nCONF=${VERIDIAN_HA_CONF:-/etc/veridian-ha.env}\n. "$CONF"\n'


class ShellTest(unittest.TestCase):
    def test_reexec_watch_misses_a_sourced_file(self):
        # 5499ffb7^: PREFERRED_PRIMARY lived in nodes.env, which the stamp didn't watch
        shipped = AGENT_HEAD + ('. "$HERE/node-${ADAPTER:-$NODE}.sh"\n[ -f "$HERE/nodes.env" ] && . "$HERE/nodes.env"\n'
                                'stamp() { stat -c %Y "$self" "$HERE/node-${ADAPTER:-$NODE}.sh"; }\nexec "$self" run\n')
        hits = L.shell_reexec_watch("deploy/ha/veridian-ha", shipped, shipped, {})
        self.assertEqual(len(hits), 1)
        self.assertIn("nodes.env", hits[0][1])
        fixed = shipped.replace('-$NODE}.sh"; }', '-$NODE}.sh" "$HERE/nodes.env"; }')
        self.assertEqual(L.shell_reexec_watch("deploy/ha/veridian-ha", fixed, fixed, {}), [])

    def test_reexec_watch_watches_another_file(self):
        # 11c09a94^: sourced node-${ADAPTER:-$NODE}.sh, watched node-$NODE.sh (never existed on Pylon)
        shipped = AGENT_HEAD + ('. "$HERE/node-${ADAPTER:-$NODE}.sh"\nstamp() { stat -c %Y "$self" "$HERE/node-$NODE.sh"; }\n'
                                'exec "$self" run\n')
        msgs = [m for _, m in L.shell_reexec_watch("x", shipped, shipped, {})]
        self.assertTrue(any("the stamp watches $HERE/node-$NODE.sh" in m for m in msgs))

    def test_name_domain(self):
        # 3088fb99^: a node named pylon-primos broke four consumers
        shipped = ('#!/bin/bash\nIP=$(eval echo "\\${${OTHER}_IP:-}")\n'
                   'to=$(tr -dc \'a-z\' < "$REQUEST_DIR/handoff")\n'
                   'psql -c "select pg_create_physical_replication_slot(\'veridian_$NODE\')"\n'
                   'm=$(tr -dc \'a-z\' < "$MODE_FILE")\n')
        lines = sorted(ln for ln, _ in L.shell_name_domain("deploy/ha/x", shipped, shipped, {}))
        self.assertEqual(lines, [2, 3, 4])  # the mode file (auto/observe) is not a name
        fixed = ('#!/bin/bash\nk=${OTHER//-/_}\nIP=$(eval echo "\\${${k}_IP:-}")\n'
                 'to=$(tr -dc \'a-z0-9-\' < "$REQUEST_DIR/handoff")\n'
                 'psql -c "select pg_create_physical_replication_slot(\'veridian_${NODE//-/_}\')"\n')
        self.assertEqual(L.shell_name_domain("deploy/ha/x", fixed, fixed, {}), [])


class CssTest(unittest.TestCase):
    def test_dropped_brace(self):
        # 51cfea01f: the Places merge dropped one closing brace in surface.css
        ok = ".a { color: red; }\n/* } */\n.b::after { content: \"}\"; }\n"
        self.assertEqual(L.css_braces("s.css", ok, ok, {}), [])
        bad = ".a { color: red;\n.b { x: 1; }\n"
        self.assertEqual([ln for ln, _ in L.css_braces("s.css", bad, bad, {})], [1])


class ProjectLintTest(unittest.TestCase):
    def project(self, files, cfg_extra=""):
        self.dir = tempfile.TemporaryDirectory()
        root = self.dir.name
        for name, body in files.items():
            path = os.path.join(root, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)
        cfg = os.path.join(root, "seamcheck.toml")
        with open(cfg, "w") as fh:
            fh.write('[project]\nname = "i"\ninclude = ["lib/**"]\n' + cfg_extra)
        L._COMPOSE_CACHE.clear()
        return core.load(root, cfg, use_cache=False)

    def tearDown(self):
        if hasattr(self, "dir"):
            self.dir.cleanup()

    COMPOSE = ("services:\n  hub:\n    image: h\n    volumes:\n"
               "      - /srv/files/elixir-pot:/home/malice/.local/share/elixir-pot\n"
               "      - \"/srv/share:/home/malice/Documents/Veridian Share Files\"\n  other:\n    image: o\n")

    def test_home_paths_mounted(self):
        # 9bf2a241^: Forms uploads and the voice pack were written inside the container
        p = self.project({
            "deploy/compose.yml": self.COMPOSE.replace('      - "/srv/share:/home/malice/Documents/Veridian Share Files"\n', ""),
            "lib/forms.ex": 'defmodule Forms do\n  def dir, do: Path.join([System.user_home!(), "Documents", "Veridian Share Files"])\n'
                            '  def pack, do: Path.expand("~/.local/share/veridian/voice-pack")\n'
                            '  def ok, do: Path.expand("~/.local/share/elixir-pot/osa")\nend\n'})
        raw = p.text("lib/forms.ex")
        hits = L.home_paths_mounted("lib/forms.ex", raw, raw, {"compose": "deploy/compose.yml"}, project=p)
        self.assertEqual(sorted(ln for ln, _ in hits), [2, 3])
        L._COMPOSE_CACHE.clear()
        with open(os.path.join(p.root, "deploy/compose.yml"), "w") as fh:
            fh.write(self.COMPOSE)
        hits = L.home_paths_mounted("lib/forms.ex", raw, raw,
                                    {"compose": "deploy/compose.yml", "allow": ["voice-pack"]}, project=p)
        self.assertEqual(hits, [])

    def test_auth_in_background(self):
        # the Robinhood connector runner retried reconnect on a timer: random approval prompts
        p = self.project({"lib/runner.ex": "defmodule Runner do\n  use GenServer\n"
                                           "  def handle_info(:tick, s) do\n    Conn.reconnect(s)\n    {:noreply, s}\n  end\n"
                                           "  def handle_info(:refresh, s), do: {:noreply, Conn.refresh(s)}\nend\n",
                          "lib/conn.ex": "defmodule Conn do\n  def reconnect(s), do: s\n  def refresh(s), do: s\nend\n"})
        raw = p.text("lib/runner.ex")
        hits = L.auth_in_background("lib/runner.ex", raw, raw, {}, project=p)
        self.assertEqual(len(hits), 1)
        self.assertIn("Conn.reconnect", hits[0][1])

    def test_stale_module_ref(self):
        # lane E removed the connector; ten comments still described it
        files = {f"lib/app/m{i}.ex": f"defmodule App.M{i} do\nend\n" for i in range(5)}
        files["lib/app/doc.ex"] = ('defmodule App.Doc do\n  @moduledoc "Talks to App.Portfolio.Connector over HTTP."\n'
                                   '  # App.Old.Thing was removed in 1.2\n  def x, do: App.M1\nend\n')
        p = self.project(files)
        raw = p.text("lib/app/doc.ex")
        code = "\n".join(p.by_file["lib/app/doc.ex"][0].code)
        hits = L.stale_module_ref("lib/app/doc.ex", raw, code, {}, project=p)
        self.assertEqual([ln for ln, _ in hits], [2])  # the history line is told as history

    def test_lockfile_sync(self):
        # tools/osa-repair-runner: lock had typescript 7.0.2, package.json ^5.9.3; npm ci refused
        p = self.project({"t/package.json": json.dumps({"devDependencies": {"typescript": "^5.9.3"}}),
                          "t/package-lock.json": json.dumps({"packages": {"": {"devDependencies": {"typescript": "^7.0.2"}}}})})
        raw = p.text("t/package-lock.json")
        self.assertEqual(len(L.lockfile_sync("t/package-lock.json", raw, raw, {}, project=p)), 1)
        with open(os.path.join(p.root, "t/package-lock.json"), "w") as fh:
            json.dump({"packages": {"": {"devDependencies": {"typescript": "^5.9.3"}}}}, fh)
        raw = p.text("t/package-lock.json")
        self.assertEqual(L.lockfile_sync("t/package-lock.json", raw, raw, {}, project=p), [])

    def test_path_unit_oneshot(self):
        # veridian-theme.path -> oneshot: picks 150 ms apart ended on the first theme
        p = self.project({"u/theme.path": "[Path]\nPathChanged=%h/.config/veridian/theme.json\n",
                          "u/theme.service": "[Service]\nType=oneshot\nExecStart=%h/.local/bin/apply\n"})
        raw = p.text("u/theme.path")
        self.assertEqual(len(L.path_unit_oneshot("u/theme.path", raw, raw, {}, project=p)), 1)
        with open(os.path.join(p.root, "u/theme.service"), "w") as fh:
            fh.write("[Service]\nType=oneshot\n# settles: re-reads theme.json until it is stable\nExecStart=apply\n")
        self.assertEqual(L.path_unit_oneshot("u/theme.path", raw, raw, {}, project=p), [])


if __name__ == "__main__":
    unittest.main()
