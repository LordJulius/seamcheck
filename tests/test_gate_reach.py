"""The gate's false blocks (python3 -m unittest discover tests). Each case is a
line agents had to waive in elixir-pot by 2026-10-19 because the gate misjudged
it: a test that runs a script found from its own folder, an HTTP test that
reaches a controller through the router, a test of a deploy file, two literal
tables taken for copies, and "shim" read inside "StashImages"."""
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core, tidy  # noqa: E402

CFG = ('[project]\nname = "t"\ninclude = ["lib/**", "test/**", "scripts/**"]\n'
       '[gate]\nforbid_added = ["fallback", "shim"]\n')
ROUTER = '''defmodule AppWeb.Router do
  use AppWeb, :router

  pipeline :api do
    plug :accepts, ["json"]
  end

  scope "/api", AppWeb do
    pipe_through :api
    get "/car", CarController, :show
    post "/car/:id/fill", CarController, :fill
  end
end
'''
CONTROLLER = '''defmodule AppWeb.CarController do
  def show(conn, _), do: Car.read(conn)
  def fill(conn, _), do: Car.fill(conn)
end
'''
CAR = '''defmodule Car do
  def read(c), do: c
  def fill(c), do: c
end
'''


def skills(name: str, ids: list[str]) -> str:
    entries = "".join(f'      %Skill{{\n        id: "{i}",\n        subject: "math",\n        title: "{i}",\n'
                      f'        group: "Linear",\n        generate: &gen/2\n      }},\n' for i in ids)
    return f"defmodule {name} do\n  def skills do\n    [\n{entries}    ]\n  end\n\n  def gen(a, b), do: a + b\nend\n"


class GateReachTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = self.dir.name
        self.write({"seamcheck.toml": CFG, "lib/router.ex": ROUTER, "lib/car_controller.ex": CONTROLLER,
                    "lib/car.ex": CAR, "scripts/lib/tool.py": "print('ok')\n",
                    "deploy/hub/compose.yml": "services: {}\n"})
        for args in (["init", "-q"], ["add", "-A"], ["commit", "-qm", "base"]):
            subprocess.run(["git", "-c", "user.email=t@t", "-c", "user.name=t", *args], cwd=self.root,
                           check=True, capture_output=True)

    def tearDown(self):
        self.dir.cleanup()

    def write(self, files):
        for name, body in files.items():
            path = os.path.join(self.root, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)

    def gate(self):
        p = core.load(self.root, os.path.join(self.root, "seamcheck.toml"), use_cache=False)
        return tidy.gate(p, "HEAD")

    def test_a_script_named_from_the_tests_folder_is_reached(self):
        # test/deploy/ota_changelog_test.exs: @tool Path.expand("../../scripts/lib/ota_changelog.py", __DIR__)
        self.write({"test/deploy/tool_test.exs": '''defmodule ToolTest do
  use ExUnit.Case
  @tool Path.expand("../../scripts/lib/tool.py", __DIR__)

  test "the tool prints ok" do
    {out, 0} = System.cmd("python3", [@tool])
    assert out == "ok\\n"
  end
end
'''})
        rep = self.gate()
        self.assertEqual(rep["tests"], [])

    def test_an_http_test_reaches_the_controller_through_the_router(self):
        # car_controller_test.exs: `conn |> get(~p"/api/car")` reached nothing
        self.write({"test/car_controller_test.exs": '''defmodule AppWeb.CarControllerTest do
  use AppWeb.ConnCase

  test "read the car back", %{conn: conn} do
    assert conn |> get(~p"/api/car") |> json_response(200)
    assert post(build_conn(), "/api/car/#{7}/fill", %{}) |> json_response(200)
  end
end
'''})
        p = core.load(self.root, os.path.join(self.root, "seamcheck.toml"), use_cache=False)
        [row] = tidy.test_audit(p)
        self.assertGreaterEqual(row["reach"], 3)  # show, fill, and the Car functions they call
        self.assertEqual(self.gate()["tests"], [])

    def test_a_test_whose_head_spans_two_lines_is_a_test(self):
        # ota_changelog_test.exs: `test "...",\n       %{repo: repo} do` was no function at all,
        # and car_controller_test.exs' `%{\n conn: conn\n } do` ended at its `} do` line
        self.write({"test/car_controller_test.exs": '''defmodule AppWeb.CarControllerTest do
  use AppWeb.ConnCase

  test "read the car back",
       %{conn: conn} do
    assert conn |> get(~p"/api/car") |> json_response(200)
  end

  test "fill the car", %{
    conn: conn
  } do
    assert post(conn, "/api/car/7/fill", %{}) |> json_response(200)
  end
end
'''})
        p = core.load(self.root, os.path.join(self.root, "seamcheck.toml"), use_cache=False)
        rows = {r["id"].rsplit(".", 1)[-1]: r["reach"] for r in tidy.test_audit(p)}
        self.assertEqual(sorted(rows), ["test fill the car", "test read the car back"])
        self.assertTrue(all(n >= 2 for n in rows.values()), rows)

    def test_an_unknown_route_still_reaches_nothing(self):
        self.write({"test/car_controller_test.exs": '''defmodule AppWeb.CarControllerTest do
  use AppWeb.ConnCase

  test "a route nobody serves", %{conn: conn} do
    assert conn |> get(~p"/api/boat") |> json_response(404)
  end
end
'''})
        self.assertEqual(len(self.gate()["tests"]), 1)

    def test_a_test_of_a_deploy_file_tests_that_file(self):
        # test/deploy/stash_lan_test.exs read compose.yml and a firewall table
        self.write({"test/deploy/lan_test.exs": '''defmodule LanTest do
  use ExUnit.Case
  @root Path.expand("../..", __DIR__)

  test "the hub has services" do
    assert File.read!(Path.join(@root, "deploy/hub/compose.yml")) =~ "services"
  end
end
'''})
        self.assertEqual(self.gate()["tests"], [])

    def test_two_literal_tables_are_not_copies(self):
        # lib/elixir_pot/learning/skills/gens/*.ex: each module lists its own skills
        self.write({"lib/eq.ex": skills("Eq", ["eq.a", "eq.b", "eq.c"]),
                    "lib/frac.ex": skills("Frac", ["fr.a", "fr.b", "fr.c"]),
                    "lib/use.ex": "defmodule Use do\n  def all, do: Eq.skills() ++ Frac.skills()\nend\n"})
        self.assertEqual(self.gate()["dups"], [])

    def test_a_forbidden_word_is_a_whole_part_of_a_name(self):
        self.write({"lib/car.ex": CAR.replace("  def fill(c), do: c\n",
                                              "  def fill(c), do: c\n  def load_stash_images(c), do: c\n"
                                              "  def loadStashImages(c), do: c\n  def use_shim(c), do: c\n")})
        self.assertEqual([f["word"] for f in self.gate()["forbidden"]], ["use_shim"])


if __name__ == "__main__":
    unittest.main()
