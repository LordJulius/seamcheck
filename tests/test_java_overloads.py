"""Java, cut down from jsoup: `public @Nullable Document ownerDocument()` is a
method (a type-use annotation among the modifiers hid it), and an override of
some overloads leaves the others to the supertype (`Element` overrides
`attr(k, v)`, so `el.attr(k)` is `Node.attr(k)`), and a project class that
extends a generic collection holds its type argument (`els.get(0)`)."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "src/main/java/org/x/Node.java": """package org.x;

public class Node {
    public String attr(String key) {
        return key;
    }

    public Node attr(String key, String value) {
        return this;
    }

    public @Nullable Document ownerDocument() {
        return null;
    }

    public String outerHtml() {
        return "";
    }
}
""",
    "src/main/java/org/x/Element.java": """package org.x;

public class Element extends Node {
    @Override public Element attr(String key, String value) {
        super.attr(key, value);
        return this;
    }
}
""",
    "src/main/java/org/x/Document.java": """package org.x;

public class Document extends Element {
    protected void outerHtml(StringBuilder accum) {
    }
}
""",
    "src/main/java/org/x/Nodes.java": """package org.x;

import java.util.ArrayList;

public class Nodes<T extends Node> extends ArrayList<T> {
    public String text() {
        return "";
    }
}
""",
    "src/main/java/org/x/Elements.java": """package org.x;

public class Elements extends Nodes<Element> {
}
""",
    "src/main/java/org/x/Use.java": """package org.x;

public class Use {
    public String one(Element el) {
        return el.attr("id");
    }

    public void two(Element el) {
        el.attr("id", "x");
    }

    public String first(Elements els) {
        els.text();
        return els.get(0).attr("id");
    }

    public String html(Document doc) {
        doc.ownerDocument();
        return doc.outerHtml();
    }
}
""",
}


class JavaOverloads(unittest.TestCase):
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

    def _to(self, fid, name):
        return {(d, e) for d, _l, e in self.p.edges.get(fid, []) if d.endswith("." + name)}

    def test_annotated_return_type(self):
        self.assertIn("src/main/java/org/x/Node.ownerDocument", self.p.funcs)
        self.assertEqual(self._to("src/main/java/org/x/Use.html", "ownerDocument"),
                         {("src/main/java/org/x/Node.ownerDocument", True)})

    def test_element_of_a_project_collection(self):
        # `Elements extends Nodes<Element>`, `Nodes<T> extends ArrayList<T>`: get(0) is an Element
        self.assertEqual(self._to("src/main/java/org/x/Use.first", "attr"), {("src/main/java/org/x/Node.attr", True)})
        self.assertEqual(self._to("src/main/java/org/x/Use.first", "text"), {("src/main/java/org/x/Nodes.text", True)})

    def test_overload_by_argument_count(self):
        self.assertEqual(self._to("src/main/java/org/x/Use.one", "attr"), {("src/main/java/org/x/Node.attr", True)})
        self.assertEqual(self._to("src/main/java/org/x/Use.two", "attr"), {("src/main/java/org/x/Element.attr", True)})
        self.assertEqual(self._to("src/main/java/org/x/Use.html", "outerHtml"), {("src/main/java/org/x/Node.outerHtml", True)})


if __name__ == "__main__":
    unittest.main()
