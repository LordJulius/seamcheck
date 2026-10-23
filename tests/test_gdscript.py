"""GDScript: `extends "relative.gd"`, `res://` paths from a Godot project in a
subfolder, and a receiver declared as an engine class (`var x: Area2D`)."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "demo/project.godot": "[application]\n",
    "demo/states/motion.gd": "extends Node\n\nfunc update_look() -> void:\n\tpass\n",
    "demo/states/ground/move.gd": 'extends "../motion.gd"\n\nfunc enter() -> void:\n\tupdate_look()\n',
    "demo/states/other.gd": "extends Node\n\nfunc update_look() -> void:\n\tpass\n",
    "demo/ball.gd": "extends Area2D\n\nfunc reset() -> void:\n\tpass\n",
    "demo/label.gd": "extends Label\n\nfunc reset() -> void:\n\tpass\n",
    "demo/wall.gd": "extends Area2D\n\nfunc _on_entered(area: Area2D) -> void:\n\tarea.reset()\n",
    "demo/game.gd": 'extends "res://states/motion.gd"\n\nfunc run() -> void:\n\tupdate_look()\n',
}


class GDScript(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.TemporaryDirectory()
        for name, body in FILES.items():
            path = os.path.join(cls.dir.name, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w") as fh:
                fh.write(body)
        cls.p = core.load(cls.dir.name, use_cache=False)

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def _edges(self, fid):
        return {(d, e) for d, _l, e in self.p.edges.get(fid, [])}

    def test_relative_extends(self):
        self.assertEqual(self._edges("move.enter"), {("motion.update_look", True)})

    def test_res_path_from_a_subfolder_project(self):
        self.assertEqual(self._edges("game.run"), {("motion.update_look", True)})

    def test_engine_typed_receiver(self):
        self.assertEqual(self._edges("wall._on_entered"), {("ball.reset", True)})


if __name__ == "__main__":
    unittest.main()
