"""[project.groups]: the project says which script a group holds (python3 -m unittest discover tests).

Battle Dynamo Blitz finds its match root by group in 185 places:
`var main := get_tree().get_first_node_in_group("match")` then `main.hum_of(team)`.
The qualifier `main` is no alias and no file stem, so every such call was a
name guess, and hotspots said Main.gd was called from 0 files. A unique name
must not become exact on its own (a second hum_of would silently retarget
every call); the project states the fact instead.
"""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "game/Main.gd": """extends Node3D

func hum_of(team: int) -> int:
	return team
""",
    "game/Other.gd": """extends Node

# a second hum_of: a name guess would hit both
func hum_of(team: int) -> int:
	return -team
""",
    "game/Hud.gd": """extends Control

func bound_here() -> void:
	var main := get_tree().get_first_node_in_group("match")
	main.hum_of(0)

func typed() -> void:
	var main: Node = hero.get_tree().get_first_node_in_group("match")
	main.hum_of(1)

func not_bound_here() -> void:
	main.hum_of(2)

func through_a_helper() -> void:
	var main := _main()
	main.hum_of(3)

func unknown_group() -> void:
	var w := get_tree().get_first_node_in_group("nobody")
	w.hum_of(4)

func _main():
	return null
""",
}


def _callees(p, fid):
    return {(dst, exact) for dst, _line, exact in p.edges.get(fid, []) if dst.endswith(".hum_of")}


class Groups(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.TemporaryDirectory()
        root = cls.dir.name
        for name, body in FILES.items():
            path = os.path.join(root, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)
        cfg = os.path.join(root, "seamcheck.toml")
        with open(cfg, "w") as fh:
            fh.write('[project]\nname = "g"\ninclude = ["**"]\n\n'
                     '[project.groups]\nmatch = "game/Main.gd"\nghost = "game/Gone.gd"\n')
        cls.p = core.load(root, cfg, use_cache=False)

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def test_bound_variable_is_exact(self):
        self.assertEqual(_callees(self.p, "Hud.bound_here"), {("Main.hum_of", True)})

    def test_typed_and_any_receiver_of_get_tree(self):
        self.assertEqual(_callees(self.p, "Hud.typed"), {("Main.hum_of", True)})

    def test_binding_is_per_function(self):
        self.assertNotIn(("Main.hum_of", True), _callees(self.p, "Hud.not_bound_here"))

    def test_helper_does_not_bind(self):
        self.assertNotIn(("Main.hum_of", True), _callees(self.p, "Hud.through_a_helper"))

    def test_unknown_group_stays_a_guess(self):
        got = _callees(self.p, "Hud.unknown_group")
        self.assertFalse(any(exact for _d, exact in got))

    def test_missing_file_is_a_config_problem(self):
        self.assertTrue(any("ghost" in m for m in self.p.config_problems))


if __name__ == "__main__":
    unittest.main()
