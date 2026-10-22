"""Swift: closures passed by label, values built through a trailing closure,
`if let` / `guard let` bindings, overloads that share a name, a test
target's types, standard-library sequence results."""
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from seamcheck import core  # noqa: E402

FILES = {
    "Package.swift": "// swift-tools-version:5.9\n",
    "Sources/Lib/Form.swift": """public final class Form {
    public func append(_ data: String) {
    }
}
""",
    "Sources/Lib/Monitor.swift": """final class Monitor {
    func append(_ x: Int) {
    }
}
""",
    "Sources/Lib/Header.swift": """public struct Header {
    public static func contentType(_ value: String) -> Header {
        Header()
    }
}

public struct Headers {
    public mutating func add(name: String, value: String) {
    }

    public mutating func add(_ header: Header) {
    }
}
""",
    "Sources/Lib/Request.swift": """public class Request {
    public func response(_ handler: @escaping () -> Void) -> Request {
        self
    }

    public func resume() {
    }
}
""",
    "Sources/Lib/Session.swift": """public class Session {
    public func upload(form: @escaping (Form) -> Void) {
    }

    public func request(_ url: String) -> Request {
        Request()
    }

    func find(for key: String) -> Request? {
        nil
    }

    func use(headers: Headers) {
        var headers = headers
        headers.add(.contentType("text/plain"))
    }

    func run(names: [String]) {
        upload(form: { form in
            form.append("a")
        })
        let request = request("u").response { print(1) }
        request.resume()
        if let found = find(for: "k") {
            found.resume()
        }
        let lowered = names.map { $0.lowercased() }
        lowered.contains("x")
    }
}
""",
    "Sources/Lib/Set.swift": """struct Bag {
    func contains(_ x: String) -> Bool {
        true
    }
}
""",
    "Tests/LibTests/Helpers.swift": """import Foundation

extension URL {
    init(fixture: String) {
        self.init(fileURLWithPath: fixture)
    }
}
""",
    "Tools/gen/Main.swift": """import Foundation

struct Main {
    func run() {
        let u = URL(fileURLWithPath: "/tmp")
        print(u)
    }
}
""",
}


def _to(p, fid, name):
    return {(dst, exact) for dst, _ln, exact in p.edges.get(fid, []) if dst.endswith("." + name)}


class Swift(unittest.TestCase):
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

    def test_labelled_closure_parameter(self):
        self.assertEqual(_to(self.p, "Sources/Lib/Session.run", "append"), {("Sources/Lib/Form.append", True)})

    def test_value_built_through_a_trailing_closure(self):
        self.assertIn(("Sources/Lib/Request.resume", True),
                      {(d, e) for d, ln, e in self.p.edges["Sources/Lib/Session.run"] if ln == 23})

    def test_if_let_binding(self):
        self.assertIn(("Sources/Lib/Request.resume", True),
                      {(d, e) for d, ln, e in self.p.edges["Sources/Lib/Session.run"] if ln == 25})

    def test_implicit_member_through_an_unlabelled_overload(self):
        self.assertIn(("Sources/Lib/Header.contentType", True),
                      {(d, e) for d, _l, e in self.p.edges["Sources/Lib/Session.use"]})

    def test_sequence_results_are_the_standard_library(self):
        self.assertEqual(_to(self.p, "Sources/Lib/Session.run", "contains"), set())

    def test_product_code_never_builds_a_test_type(self):
        self.assertEqual({d for d, _l, _e in self.p.edges.get("Tools/gen/Main.run", [])}, set())


if __name__ == "__main__":
    unittest.main()
