"""Framework rule packs (python3 -m unittest discover tests).

Every Phoenix app needed the same three OTP lints, every React app the same
UI lints, and each project copied them into its own config (or never did).
`[project] packs = ["phoenix"]` turns on seamcheck/packs/phoenix.toml. Each
pack must run clean on correct code and fire on the cut-down bugs its lints
were written after; a project overrides a pack rule by its id, and a pack
name nothing ships is a config error, not a silently empty pack.
"""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import config, init  # noqa: E402
from test_lint import (BAR_TSX, CSS_AFTER, CSS_BEFORE, PLACES_AFTER, PLACES_BEFORE, PMTILES_AFTER,  # noqa: E402
                       PMTILES_BEFORE, SPOTIFY_AFTER, SPOTIFY_BEFORE, STOCKS_AFTER, STOCKS_BEFORE, VM_AFTER,
                       VM_BEFORE, run)
from test_lint_misses import FILES as QML_FILES  # noqa: E402

RESCUE = ("defmodule Player do\n  require Logger\n\n  def poll do\n    save()\n  rescue\n"
          "    e ->\n      Logger.debug(\"poll failed: #{Exception.message(e)}\")\n      :not_playing\n  end\nend\n")

GOOD = {
    "phoenix": {**PMTILES_AFTER, "lib/stocks.ex": STOCKS_AFTER, "lib/player.ex": RESCUE.replace("debug", "warning"),
                "lib/params.ex": "defmodule Params do\n  def key(s), do: String.to_existing_atom(s)\nend\n"},
    "react": {"src/PlaceCard.tsx": PLACES_AFTER, "src/CommandInput.tsx": BAR_TSX, "styles/surface.css": CSS_AFTER,
              "src/useFeed.ts": "export function useFeed() {\n  useEffect(() => {\n    load();\n  }, []);\n}\n",
              "src/Counter.jsx": ("class Counter extends React.Component {\n  constructor(props) {\n    super(props);\n"
                                  "    this.state = { n: 0 };\n  }\n  bump() {\n    this.setState({ n: this.state.n + 1 });\n"
                                  "    return this.state.n == 1;\n  }\n}\n")},
    "qml": {**QML_FILES, "qml/Spotify.qml": SPOTIFY_AFTER,
            "qml/VButton.qml": "import QtQuick\nimport VeridianOsd\nRectangle {\n  radius: Tokens.gap\n}\n"},
    "express": {"package.json": '{"name": "shop", "dependencies": {"express": "^5"}}',
                "app.js": "const express = require('express')\nconst app = express()\n"
                          "app.get('/', function (req, res) {\n  res.send('hi')\n})\n"},
    "hono": {"package.json": '{"name": "api", "dependencies": {"hono": "^4"}}',
             "src/index.ts": "import { Hono } from 'hono'\nconst app = new Hono()\napp.get('/', (c) => c.text('hi'))\n"},
    # request data that goes only through a parameterized query, an int(), a sanitizer, or a fixed command
    "security": {"app.py": ("from flask import request\nimport subprocess\n\n\ndef show(db):\n"
                            "    uid = request.args.get(\"id\")\n    db.execute(\"SELECT * FROM t WHERE id = ?\", (uid,))\n"
                            "    n = int(request.args.get(\"n\"))\n    subprocess.run(\"seq %d\" % n, shell=True)\n"
                            "    subprocess.run(\"date\")\n    log(uid)\n\n\ndef log(text):\n    print(text)\n"),
                 "server.js": ("const { exec } = require(\"child_process\");\n"
                               "function list(req, res) {\n  const dir = sanitizePath(req.query.dir);\n  exec(\"ls \" + dir);\n"
                               "  db.query(\"SELECT * FROM t WHERE id = $1\", [req.params.id]);\n  res.redirect(\"/home\");\n}\n"),
                 "page.php": "<?php\n$id = intval($_GET['id']);\nmysqli_query($db, \"SELECT * FROM t WHERE id = $id\");\n",
                 "app/controllers/users_controller.rb": ("class UsersController\n  def show\n"
                                                         "    @user = User.where(id: params[:id]).first\n"
                                                         "    redirect_to :login\n  end\nend\n"),
                 "tests/test_app.py": "import subprocess\nfrom flask import request\n\n\ndef test_x():\n"
                                      "    subprocess.run(request.args[\"c\"], shell=True, verify=False)\n",
                 # a slow hash, a verified token, debug from the environment, a file digest
                 "auth.py": ("import bcrypt, hashlib, jwt, os\n\n\ndef store(password):\n"
                             "    return bcrypt.hashpw(password, bcrypt.gensalt())\n\n\ndef user(token, key):\n"
                             "    return jwt.decode(token, key, algorithms=['HS256'])\n\n\ndef digest(data):\n"
                             "    return hashlib.sha256(data).hexdigest()\n\n\n"
                             "def main(app):\n    app.run(debug=os.environ.get('DEBUG') == '1')\n")},
    "compose": {"app/src/main/java/LauncherViewModel.kt": VM_AFTER,
                "app/src/main/java/Sync.kt": "class Sync(val vm: VM) {\n  fun start() {\n    vm.viewModelScope.launch { run() }\n  }\n}\n",
                "app/src/test/java/SyncTest.kt": "fun go() {\n  GlobalScope.launch { }\n}\n"},
}
# the bugs, cut down: rule (without the pack prefix) -> files that hold it
BAD = {
    "phoenix": {"ets-owner": PMTILES_BEFORE, "handle-info-catch-all": {"lib/stocks.ex": STOCKS_BEFORE},
                "rescue-logged-at-debug": {"lib/player.ex": RESCUE},
                "no-atoms-from-strings": {"lib/params.ex": "defmodule Params do\n  def key(s), do: String.to_atom(s)\nend\n"}},
    "react": {"inert-control": {"src/PlaceCard.tsx": PLACES_BEFORE},
              "disabled-by-data": {"src/Rows.tsx": "export function Rows({ items }) {\n  return items.map((n) => (\n"
                                   "    <button className=\"notice-row\" disabled={n.read} onClick={() => open(n)}>x</button>\n"
                                   "  ));\n}\n"},
              "popup-clipped": {"src/CommandInput.tsx": BAR_TSX, "styles/surface.css": CSS_BEFORE},
              "effect-not-async": {"src/useFeed.ts": "export function useFeed() {\n  useEffect(async () => {\n"
                                   "    await load();\n  }, []);\n}\n"},
              "state-not-assigned": {"src/Counter.jsx": "class Counter extends React.Component {\n  bump() {\n"
                                     "    this.state.n = this.state.n + 1;\n  }\n}\n"}},
    "qml": {"anchors-in-positioner": {"qml/Spotify.qml": SPOTIFY_BEFORE},
            "singleton-import": {**QML_FILES, "qml/VButton.qml": "import QtQuick\nRectangle {\n  radius: Tokens.gap\n}\n"},
            "js-shadows-type": {**QML_FILES, "qml/HubMove.js": ".pragma library\nfunction ok() { return 1 }\n"},
            "unknown-type": {**QML_FILES, "qml/Main.qml": "import QtQuick\nItem {\n  Sheet { }\n}\n"}},
    "security": {
        # the value reaches the shell through a helper's parameter
        "python-injection": {"app.py": ("from flask import request\nimport subprocess\n\n\ndef ping():\n"
                                        "    host = request.args.get(\"host\")\n    run(host)\n\n\n"
                                        "def run(h):\n    subprocess.run(\"ping \" + h, shell=True)\n")},
        "js-injection": {"server.js": ("const { exec } = require(\"child_process\");\n"
                                       "function ping(req, res) {\n  const host = req.query.host;\n  exec(`ping ${host}`);\n}\n")},
        "php-injection": {"page.php": "<?php\n$id = $_GET['id'];\nmysqli_query($db, \"SELECT * FROM t WHERE id = $id\");\n"},
        "ruby-injection": {"app/controllers/users_controller.rb": ("class UsersController\n  def show\n"
                                                                   "    @user = User.where(\"id = #{params[:id]}\").first\n"
                                                                   "  end\nend\n")},
        "go-injection": {"main.go": ("package main\n\nimport (\n\t\"net/http\"\n\t\"os/exec\"\n)\n\n"
                                     "func handler(w http.ResponseWriter, r *http.Request) {\n"
                                     "\thost := r.URL.Query().Get(\"host\")\n\texec.Command(\"sh\", \"-c\", \"ping \"+host).Run()\n}\n")},
        "java-injection": {"src/Ping.java": ("public class Ping {\n    public void doGet(HttpServletRequest request) throws Exception {\n"
                                             "        String host = request.getParameter(\"host\");\n"
                                             "        Runtime.getRuntime().exec(\"ping \" + host);\n    }\n}\n")},
        "tls-verification-off": {"client.py": "import requests\n\n\ndef get(u):\n    return requests.get(u, verify=False)\n"},
        "flask-debug": {"app.py": "from flask import Flask\napp = Flask(__name__)\napp.run(host='0.0.0.0', debug=True)\n"},
        "xml-external-entities": {"x.js": "const libxmljs = require('libxmljs');\n"
                                          "function load(s) {\n  return libxmljs.parseXmlString(s, {noent: true});\n}\n"},
        "weak-password-hash": {"users.py": "import hashlib\n\n\ndef store(password):\n"
                                           "    return hashlib.md5(password.encode()).hexdigest()\n"},
        "jwt-unverified": {"auth.py": "import jwt\n\n\ndef user(token):\n    return jwt.decode(token, verify=False)\n"},
        "csharp-injection": {"PingController.cs": ("public class PingController {\n    public void Get() {\n"
                                                   "        var host = Request.Query[\"host\"];\n"
                                                   "        Process.Start(\"cmd.exe\", \"/c ping \" + host);\n    }\n}\n")}},
    "compose": {"init-order": {"app/src/main/java/LauncherViewModel.kt": VM_BEFORE},
                "inert-control": {"app/src/main/java/Bar.kt": "@Composable\nfun Bar() {\n  IconButton(onClick = {}) { }\n}\n"},
                "no-global-scope": {"app/src/main/java/Sync.kt": "class Sync {\n  fun start() {\n"
                                    "    GlobalScope.launch { run() }\n  }\n}\n"}},
}


def with_packs(packs: str, files: dict, rules: str = "") -> dict:
    return run(files, f"packs = {packs}\n" + rules)


class PackTest(unittest.TestCase):
    def test_every_pack_is_tested(self):
        self.assertEqual(sorted(GOOD), config.available_packs())

    def test_packs_load_and_run_clean_on_correct_code(self):
        for pack, files in GOOD.items():
            with self.subTest(pack=pack):
                self.assertEqual(with_packs(f'["{pack}"]', files), {})

    def test_each_rule_fires_on_its_bug(self):
        for pack, cases in BAD.items():
            with open(os.path.join(config.PACKS_DIR, f"{pack}.toml"), "rb") as fh:
                ids = {r["id"] for r in config.tomllib.load(fh)["rule"]}
            self.assertEqual(set(cases), ids, f"{pack}: a rule without its bug")
            for rule, files in cases.items():
                with self.subTest(pack=pack, rule=rule):
                    got = with_packs(f'["{pack}"]', files)
                    self.assertNotIn("config", got)
                    self.assertIn(f"{pack}/{rule}", got)

    def test_a_project_overrides_a_pack_rule_by_its_id(self):
        bad = BAD["phoenix"]["handle-info-catch-all"]
        override = '\n[[rule]]\nid = "phoenix/handle-info-catch-all"\nseverity = "warn"\n'
        with tempfile.TemporaryDirectory() as root:
            with open(os.path.join(root, "seamcheck.toml"), "w") as fh:
                fh.write('[project]\npacks = ["phoenix"]\n' + override)
            cfg, problems = config.load(root)
        self.assertEqual(problems, [])
        rule = next(r for r in cfg["rule"] if r["id"] == "phoenix/handle-info-catch-all")
        self.assertEqual((rule["severity"], rule["lint"], rule["_file"]), ("warn", "handle-info-catch-all", "seamcheck.toml"))
        self.assertEqual(len(with_packs('["phoenix"]', bad, override)["phoenix/handle-info-catch-all"]), 1)

    def test_unknown_packs_and_overrides_are_config_problems(self):
        with tempfile.TemporaryDirectory() as root:
            with open(os.path.join(root, "seamcheck.toml"), "w") as fh:
                fh.write('[project]\npacks = ["phoenx", "react"]\n\n[[rule]]\nid = "react/no-such"\nseverity = "warn"\n'
                         '\n[[rule]]\nid = "qml/unknown-type"\nseverity = "warn"\n')
            _, problems = config.load(root)
        self.assertEqual(len(problems), 3, problems)
        self.assertIn("unknown pack 'phoenx' (did you mean 'phoenix'?)", problems[0])
        self.assertIn("pack react has no rule 'no-such'", " ".join(problems))
        self.assertIn("overrides pack qml, which [project] packs doesn't list", " ".join(problems))


class InitDetectsPacks(unittest.TestCase):
    def test_init_writes_the_frameworks_it_finds(self):
        files = {
            "hub/mix.exs": 'defp deps do\n  [{:phoenix, "~> 1.7"}, {:jason, "~> 1.4"}]\nend\n',
            "hub/lib/app.ex": "defmodule App do\nend\n",
            "web/package.json": '{"dependencies": {"react": "^18.3.1", "react-dom": "^18.3.1"}}',
            "web/src/main.tsx": "export const x = 1;\n",
            "desk/CMakeLists.txt": "find_package(Qt6 REQUIRED COMPONENTS Quick)\n",
            "android/app/build.gradle.kts": "android {\n  buildFeatures { compose = true }\n}\n",
            "tools/package.json": '{"dependencies": {"preact": "^10"}}',
        }
        with tempfile.TemporaryDirectory() as root:
            for name, body in files.items():
                os.makedirs(os.path.dirname(os.path.join(root, name)), exist_ok=True)
                with open(os.path.join(root, name), "w") as fh:
                    fh.write(body)
            # a CMake project that links Qt Quick but has no QML is not a QML app
            self.assertEqual(init.detect_packs(root, sorted(files)), ["compose", "phoenix", "react"])
            with open(os.path.join(root, "desk/Main.qml"), "w") as fh:
                fh.write("import QtQuick\nItem { }\n")
            text, summary = init.render(root)
            self.assertIn('packs = ["compose", "phoenix", "qml", "react", "security"]', text)
            self.assertIn("rule packs: compose, phoenix, qml, react, security", summary)
            with open(os.path.join(root, "seamcheck.toml"), "w") as fh:
                fh.write(text)
            cfg, problems = config.load(root)
            self.assertEqual(problems, [])
            self.assertIn("phoenix/ets-owner", {r["id"] for r in cfg["rule"]})


class ExpressParams(unittest.TestCase):
    """The express pack's [[param]]: a handler's `res` is express's response,
    detected from package.json without being listed."""

    def load(self, files):
        from seamcheck import core
        d = tempfile.TemporaryDirectory()
        for name, body in files.items():
            path = os.path.join(d.name, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)
        self.addCleanup(d.cleanup)
        return core.load(d.name, use_cache=False)

    def test_in_an_app_res_send_is_the_librarys(self):
        p = self.load({"package.json": '{"name": "shop", "dependencies": {"express": "^5"}}',
                       "mail.js": "function send(to) {}\nmodule.exports = { send }\n",
                       "app.js": "const app = require('express')()\n"
                                 "function routes() {\n  app.get('/', (req, res, next) => {\n    res.send('hi')\n  })\n}\n"})
        self.assertEqual(p.edges.get("app.routes", []), [])  # not a guess at mail.send

    def test_in_express_itself_res_is_lib_response(self):
        p = self.load({"package.json": '{"name": "express"}',
                       "lib/response.js": "var res = module.exports = {}\nres.send = function send(body) {}\n",
                       "test/res.send.js": "function t() {\n  app.use(function (req, res) {\n    res.send(1)\n  })\n}\n"})
        self.assertEqual([(d, ex) for d, _, ex in p.edges.get("test/res.send.t", [])], [("lib/response.send", True)])


class HonoParams(ExpressParams):
    def test_in_an_app_route_context_is_the_librarys(self):
        p = self.load({"package.json": '{"name": "api", "dependencies": {"hono": "^4"}}',
                       "src/util.ts": "export function json(x: unknown) { return x }\n",
                       "src/routes.ts": "export function routes(app: any) {\n  app.get('/', (c) => c.json({}))\n"
                                        "  return [1].map((c) => c.json)\n}\n"})
        self.assertEqual(p.edges.get("src/routes.routes", []), [])  # c.json is hono's, not util.json

    def test_the_rpc_clients_responses_are_the_platforms(self):
        p = self.load({"package.json": '{"name": "api", "dependencies": {"hono": "^4"}}',
                       "src/util.ts": "export function json(x: unknown) { return x }\n",
                       "src/use.ts": "import { hc } from 'hono/client'\nexport async function go() {\n"
                                     "  const client = hc('/')\n  const res = await client.posts.$get()\n  res.json()\n}\n"})
        self.assertEqual(p.edges.get("src/use.go", []), [])  # not a guess at util.json

    def test_in_hono_itself_c_is_src_context(self):
        p = self.load({"package.json": '{"name": "hono"}',
                       "src/context.ts": "export class Context {\n  json(o: object) { return o }\n}\n",
                       "src/mw.ts": "export function mw() {\n  return async function mw(c, next) {\n    c.json({})\n  }\n}\n"})
        got = [(d, ex) for f in p.funcs if f.startswith("src/mw.") for d, _, ex in p.edges.get(f, [])]
        self.assertEqual(got, [("src/context.json", True)])


if __name__ == "__main__":
    unittest.main()


class SecurityFlowCases(unittest.TestCase):
    """Injections CodeQL found on railsgoat / NodeGoat that the pack missed, and the
    sinatra code its first fix flagged by mistake."""

    def found(self, files):
        got = with_packs('["security"]', files)
        return [m for ms in got.values() for _f, _l, m in ms if "reaches" in m]

    def test_a_sanitizer_inside_a_string_cleans_only_its_part(self):
        ctrl = "class FormsController\n  def upload\n    file = params[:upload]\n    Benefits.save(file)\n  end\nend\n"
        model = ("class Benefits\n  def self.save(file)\n    name = \"/data/#{file.original_filename}\"\n"
                 "    system(\"cp #{name} /bak/#{Time.now.to_i}\")\n  end\nend\n")
        self.assertTrue(any("system()" in m for m in self.found({"app/controllers/forms_controller.rb": ctrl,
                                                                 "app/models/benefits.rb": model})))
        safe = ("class App\n  def image\n    filename = \"/images/#{params[:image].to_i}.png\"\n"
                "    send_file filename\n  end\nend\n")
        self.assertEqual([], self.found({"app/controllers/app_controller.rb": safe}))

    def test_a_rails_scope_lambda_is_a_method(self):
        model = "class Analytics\n  scope :hits_by_ip, ->(ip, col = \"*\") { select(\"#{col}\").where(ip_address: ip) }\nend\n"
        ctrl = "class AdminController\n  def analytics\n    Analytics.hits_by_ip(params[:ip], params[:field])\n  end\nend\n"
        self.assertTrue(any("hits_by_ip" in m for m in self.found({"app/models/analytics.rb": model,
                                                                   "app/controllers/admin_controller.rb": ctrl})))

    def test_a_query_a_closure_builds_from_a_tainted_name(self):
        dao = ("function AllocationsDAO(db) {\n  const col = db.collection('allocations');\n"
               "  this.byThreshold = (userId, threshold, callback) => {\n    const criteria = () => {\n"
               "      return {\n        $where: `this.stocks > '${threshold}'`\n      };\n    };\n"
               "    col.find(criteria()).toArray(callback);\n  };\n}\nmodule.exports = { AllocationsDAO };\n")
        route = ("const { AllocationsDAO } = require('../data/allocations-dao');\nfunction Handler(db) {\n"
                 "  const dao = new AllocationsDAO(db);\n  this.show = (req, res) => {\n"
                 "    dao.byThreshold(req.params.userId, req.query.threshold, (e, r) => res.send(r));\n  };\n}\n"
                 "module.exports = Handler;\n")
        self.assertTrue(any("find()" in m for m in self.found({"app/data/allocations-dao.js": dao, "app/routes/a.js": route})))
