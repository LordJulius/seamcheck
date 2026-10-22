"""TypeScript: generic / multi-line method signatures, `type X = Map<...>`
aliases, destructuring, the global Window a project augments."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "package.json": '{"name": "t", "type": "module"}',
    "src/scene.ts": """import type { ElementsMap } from "./types";

export class Scene {
  mutateElement<TElement extends Mutable<Element>>(
    element: TElement,
    updates: { a?: number; b?: string },
  ): TElement & {
    get(): TElement;
  } {
    return element as any;
  }

  getMap(): ElementsMap {
    return new Map();
  }
}
""",
    "src/types.ts": """export type ElementsMap = Map<string, number> & { __brand: "x" };
""",
    "src/cache.ts": """export class Cache {
  get(key: string) {
    return key;
  }
}
""",
    "src/app.ts": """import { Scene } from "./scene";

export class App {
  scene: Scene = new Scene();
}

declare global {
  interface Window {
    h: {
      app: InstanceType<typeof App>;
      scene: Scene;
    };
  }
}
""",
    "src/use.ts": """import { Scene } from "./scene";
import type { ElementsMap } from "./types";

const { h } = window;

export function mutate(scene: Scene) {
  scene.mutateElement(null, {});
}

export function lookup(map: ElementsMap) {
  map.get("a");
}

export function hook() {
  h.app.scene.mutateElement(null, {});
}

export function destructured(app: { scene: Scene }) {
  const { scene } = app;
  scene.getMap();
}
""",
}


def _to(p, fid, name):
    return {(dst, exact) for dst, _ln, exact in p.edges.get(fid, []) if dst.endswith("." + name)}


class TypeScript(unittest.TestCase):
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

    def test_generic_multiline_method_is_found(self):
        self.assertIn("src/scene.mutateElement", self.p.funcs)
        self.assertEqual(_to(self.p, "src/use.mutate", "mutateElement"), {("src/scene.mutateElement", True)})

    def test_alias_of_a_runtime_type_is_the_library(self):
        self.assertEqual(_to(self.p, "src/use.lookup", "get"), set())

    def test_window_augmentation_and_destructuring(self):
        self.assertEqual(_to(self.p, "src/use.hook", "mutateElement"), {("src/scene.mutateElement", True)})


if __name__ == "__main__":
    unittest.main()
