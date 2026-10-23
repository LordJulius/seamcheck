"""Ruby adapter: one Module per file, and one per class or module
(`<file>.<A::B>`, nested names spelled out), like Python's classes.

Ruby writes down less than other languages: a call needs no parentheses
(`render json: x`, `site.config`), a bare name is a method call unless the
method assigned it first, a constant means whatever the lexical scope (then
the superclass) finds wherever it was defined, and no parameter has a type.
So the parse records what each file says, and `link` (run by core once
every file is parsed) resolves it across the project: `require`s to files,
constants to their class modules, `< Base` and `include M` to supertypes,
and the types of parameters and fields from what every caller passes. A
class reopened in several files is one class: one definition (the one with
`< Base`, else the file named after it) is the class, the others `ext`
modules linked both ways, so a method from any of them is found on it.
"""
from __future__ import annotations

import bisect
import os
import re

from .chains import type_name
from .model import Call, Func, Module
from .scan import blank

EXT = (".rb", ".rake", ".ru")

_ID = r"[A-Za-z_]\w*"
_CONST = r"(?:::)?[A-Z]\w*(?:::[A-Z]\w*)*"
# block openers / closers; not `x.class`, `:end`, `@do`, `end:` (a hash key), `end?`
_TOK = re.compile(r"(?<![\w.$@:])(class|module|def|if|unless|while|until|case|begin|for|do|end)(?![\w?!]|:(?!:))")
_DEF = re.compile(rf"def\s+(?:(self|{_CONST}|@{{0,2}}[a-z_]\w*)\s*\.\s*)?({_ID}[?!=]?|\[\]=?|===?|<=>|=~|![=~]?|[-+*/%<>~^&|]+@?|`)")
_DEF_ANY = re.compile(r"def\s+()(\S+)")
_CLASS = re.compile(rf"class\s+({_CONST})(?:\s*<\s*({_CONST}))?")
_SCLASS = re.compile(r"class\s*<<\s*\w+")
_MODULE = re.compile(rf"module\s+({_CONST})")
# words before a `if` / `unless` / `while` / `until` that make it open a block
# (`x = if c`), not a modifier (`return if c`, `foo(a) unless b`)
_OPENS_AFTER = {"then", "else", "do", "begin", "and", "or", "not", "when", "in", "elsif"}
# do-blocks that are something: a test, a method, a class
_TEST_HEAD = re.compile(r"^\s*(?:it|specify|example|scenario|test|should|its)\b\s*\(?\s*(?:[\"':]|$|do\b)")
_DEFINE_METHOD = re.compile(r"\bdefine_method\s*\(?\s*(?::|[\"'])(\w+[?!=]?)")
_CLASS_NEW = re.compile(rf"^\s*([A-Z]\w*)\s*=\s*(?:(Class|Struct|Data)\.(?:new|define))\b\s*(?:\(\s*({_CONST}))?")
_ANON_CLASS = re.compile(rf"(?<![\w:])(Class|Module)\.new\b\s*(?:\(\s*({_CONST})\s*\))?\s*$")
_MIXIN = re.compile(rf"^\s*(include|extend|prepend)\s+((?:{_CONST})(?:\s*,\s*{_CONST})*)\s*$")
_ATTR = re.compile(r"^\s*attr_(reader|accessor)\b\s*\(?")
_DELEGATORS = re.compile(r"^\s*def_delegators?\s+:(@?\w+)\s*,\s*(.*)$")
_RAILS_DELEGATE = re.compile(r"^\s*delegate\s+(.*?)\bto:\s*:(@?\w+)")
_LET = re.compile(r"^\s*(?:let!?\s*\(?\s*:(\w+[?!]?)\s*\)?|subject)\s*(\{|do\b)\s*")
_PRIVATE = re.compile(r"^\s*(private|protected|public)\s*$")
_REQUIRE = re.compile(r"""^\s*(require|require_relative|load)\s*\(?\s*["']([^"'#]+)["']""")
_AUTOLOAD = re.compile(r"""^\s*autoload\s*\(?\s*:\w+\s*,\s*["']([^"'#]+)["']""")
_ASSIGN = re.compile(r"(?<![\w.@$:])([a-z_]\w*)\s*(?:\|\||&&|\*\*|<<|>>|[-+*/%|&^])?=(?![=~>])")
_MULTI = re.compile(r"^\s*((?:\*?@{0,2}[a-z_]\w*\s*,\s*)+\*?@{0,2}[a-z_]\w*)\s*=(?![=~>])", re.M)
# `<receiver>.each do |x|` / `.map { |x| ... }`: x is an element of the receiver
_ITERS = {"each", "map", "select", "find", "detect", "each_with_index", "sort_by", "reject", "any?", "all?",
          "flat_map", "filter", "filter_map", "count", "min_by", "max_by", "group_by", "partition", "each_with_object",
          "sum", "find_all", "none?", "map!", "select!", "reject!", "collect", "each_slice", "find_index", "index_by",
          "to_h", "sort", "uniq"}
_SAME_ELEMENTS = {"sort", "sort_by", "select", "reject", "filter", "uniq", "reverse", "compact", "to_a", "values",
                  "dup", "flatten", "select!", "reject!", "sort!", "uniq!", "compact!", "first", "last", "take", "drop"}
_ITER_BLOCK = re.compile(r"((?:@{0,2}[a-z_]\w*|[A-Z][\w:]*)(?:\[[^\]\n]*\])?(?:\s*\.\s*[a-z_]\w*[?!]?(?:\([^()\n]*\))?(?:\[[^\]\n]*\])?)*?)"
                         r"\s*\.\s*(?:each|map|select|find|detect|each_with_index|sort_by|reject|any\?|all\?|flat_map|filter|"
                         r"filter_map|count|min_by|max_by|group_by|partition|sum|find_all|none\?|map!|select!|reject!|"
                         r"collect|find_index)\b(?:\([^()\n]*\))?\s*(?:\bdo|\{)\s*\|([^|\n]*)\|")
_BLOCK_PARAMS = re.compile(r"(?:\bdo|\{)\s*\|([^|\n]*)\|")
# `Benchmark.ips do |x|`, `File.open(p) { |f| }`: a block given to a constant's method
_CONST_BLOCK = re.compile(rf"(?<![\w.:])({_CONST})\s*\.\s*\w+[?!]?(?:\s*\(([^()\n]*)\)|[ \t]+([^\n{{|]*?))?\s*(?:\bdo|\{{)\s*\|([^|\n]*)\|")
# Ruby's own constants whose blocks get Ruby's objects (`File.open(path) do |f|`);
# another library's may hand back what it was given (`Parallel.map(pages) { |page| }`)
_STDLIB_YIELDS = {"File", "Dir", "IO", "Benchmark", "Open3", "Tempfile", "Zlib", "Timeout", "Mutex", "Thread",
                  "OptionParser", "CSV", "Find", "FileUtils", "Pathname", "StringIO", "Socket", "TCPServer",
                  "Net", "Etc", "Process", "Signal", "Logger", "Dir::Tmpname", "ERB"}
_RESCUE_VAR = re.compile(r"\brescue\b[^\n=]*=>\s*([a-z_]\w*)")
_RESCUE_BIND = re.compile(rf"\brescue\b\s*((?:{_CONST})(?:\s*,\s*{_CONST})*)?\s*=>\s*([a-z_]\w*)")
_SELF_SET = re.compile(r"(?<![\w@.])self\.(\w+)\s*=(?![=~>])\s*")
_FOR_VAR = re.compile(r"\bfor\s+([a-z_]\w*(?:\s*,\s*[a-z_]\w*)*)\s+in\b")
_IVAR_SET = re.compile(r"(?<![\w@])(@\w+)\s*(?:\|\|)?=(?![=~>])\s*")
_WORD = re.compile(r"(?<![\w@$])([A-Za-z_]\w*[?!]?)")
_ALIAS = re.compile(r"alias\s+:?([\w?!=]+)\s+:?([\w?!=]+)\s*$")
_SYMBOL = re.compile(r"(?<![:\w]):(@{0,2}[A-Za-z_]\w*[?!=]?)")
_YARD_RET = re.compile(r"^\s*#\s*@return\s+\[([^\]]+)\]")
_YARD_PARAM = re.compile(r"^\s*#\s*@param\s+\[?(\w+)\]?\s+\[([^\]]+)\]")
_YARD_PARAM2 = re.compile(r"^\s*#\s*@param\s+\[([^\]]+)\]\s+(\w+)")
_SIG_RET = re.compile(r"\.returns\(\s*([\w:]+)")
_SIG_PARAM = re.compile(r"(\w+):\s*([A-Z][\w:]*)")

_KEYWORDS = {
    "alias", "and", "begin", "break", "case", "class", "def", "defined?", "do", "else", "elsif", "end", "ensure",
    "false", "for", "if", "in", "module", "next", "nil", "not", "or", "redo", "rescue", "retry", "return", "self",
    "super", "then", "true", "undef", "unless", "until", "when", "while", "yield", "__FILE__", "__LINE__",
    "__dir__", "__method__", "__ENCODING__", "__callee__", "block_given?", "BEGIN", "END",
}
# Ruby's own declarations written as calls (`include M`, `attr_reader :x`,
# `private`, `require "x"`) and Kernel's everyday ones (`puts`, `raise`): not
# project code, and recording them only made noise.
_DECLARE = {
    "require", "require_relative", "load", "autoload", "include", "extend", "prepend", "attr_reader", "attr_writer",
    "attr_accessor", "attr", "private", "public", "protected", "module_function", "private_constant",
    "private_class_method", "public_class_method", "alias_method", "define_method", "def_delegator",
    "def_delegators", "delegate", "raise", "fail", "lambda", "proc", "loop", "puts", "print", "p", "pp",
    "catch", "throw", "binding", "caller", "sig", "using", "refine",
}
_TEST_WORDS = {"it", "specify", "example", "scenario", "test", "should", "its", "describe", "context", "feature"}
# test-framework blocks (`context "x" do`, `setup do`): grouping and fixtures, not project calls
_DSL_BLOCKS = _TEST_WORDS | {"setup", "teardown", "shared_examples", "shared_context", "shared_examples_for",
                             "let", "let!", "subject", "before", "after", "around"}
# a literal's class: its methods are Ruby's own
_PCT_TYPE = {"w": "Array", "W": "Array", "i": "Array", "I": "Array", "q": "String", "Q": "String", "": "String",
             "r": "Regexp", "s": "Symbol", "x": "String"}
_LITERAL_WORDS = {"nil": "NilClass", "true": "TrueClass", "false": "FalseClass"}
# Object / Kernel / Enumerable / Hash / Array / String methods: on a receiver
# the code never types (a parameter, a block's `|x|`, an untyped field) they
# are Ruby's own, not a guess at each project class that also defines one
# (as core does for Rust's `.unwrap()` / `.len()`).
_CORE_METHODS = set("""
to_s inspect eql? hash is_a? kind_of? instance_of? respond_to? send public_send nil? dup clone freeze frozen?
tap then object_id instance_variable_get instance_variable_set itself
each each_with_index each_with_object each_pair each_key each_value map flat_map collect select filter reject find
detect find_index include? member? any? all? none? count first last min max sort sort_by group_by partition reduce
inject sum to_a to_h to_hash to_ary zip take drop uniq compact flatten length size empty? key? has_key? keys values
fetch dig merge merge! delete push pop shift unshift concat join split strip gsub sub start_with? end_with? downcase
upcase to_sym to_i to_f to_str match match? scan force_encoding encoding values_at slice tr chomp lstrip rstrip
sort! map! select! reject! uniq! compact! each_char bytes chars lines""".split())


def _modname(rel: str) -> str:
    return rel.rsplit(".", 1)[0]


def _test_file(rel: str) -> bool:
    base = rel.rsplit("/", 1)[-1]
    return (base.endswith(("_test.rb", "_spec.rb")) or base.startswith("test_")
            or "/test/" in "/" + rel or "/spec/" in "/" + rel)


# --- blocks -------------------------------------------------------------------

def _match_paren(code: str, k: int) -> int:
    """code[k] is "(": the index of its ")" (or len(code))."""
    depth = 0
    n = len(code)
    while k < n:
        if code[k] in "([{":
            depth += 1
        elif code[k] in ")]}":
            depth -= 1
            if depth == 0:
                return k
        k += 1
    return n


def _stmt_prefix(code: str, pos: int) -> str:
    a = max(code.rfind("\n", 0, pos), code.rfind(";", 0, pos)) + 1
    return code[a:pos].rstrip()


def _opens(code: str, pos: int) -> bool:
    """An `if`/`unless`/`while`/`until` at pos opens a block (it leads a
    statement or a value), rather than modifying the statement before it."""
    pre = _stmt_prefix(code, pos)
    if not pre or pre[-1] in "=([{,|&<>+-%~^":  # `host << if c`: an operand (not `/re/ unless`)
        return True
    if pre[-1] in "?!:" and (len(pre) < 2 or pre[-2] in " \t"):  # `a ? if`, not `empty? if`
        return True
    m = re.search(r"(\w+)$", pre)
    return bool(m) and m.group(1) in _OPENS_AFTER and not pre[:m.start()].endswith(".")


def _blocks(code: str, line_of) -> list[dict]:
    """Every `end`-closed block: kind ("class", "module", "sclass", "def",
    "test", "do", "other"), first and last line (0-based), and what its head
    says (name, superclass, label position)."""
    out: list[dict] = []
    stack: list[dict] = []
    loop_do = -1  # a `while x do` line: its `do` belongs to the loop
    skip_to = 0
    for m in _TOK.finditer(code):
        pos = m.start()
        if pos < skip_to:
            continue
        kw = m.group(1)
        line = line_of(pos)
        b: dict | None = None
        if kw == "end":
            if stack:
                top = stack.pop()
                top["b"] = line
                top["end_pos"] = pos
            continue
        if kw == "def":
            dm = _DEF.match(code, pos) or _DEF_ANY.match(code, pos)  # unreadable: still a block `end` closes
            k = dm.end()
            while k < len(code) and code[k] in " \t":
                k += 1
            params_at = k
            if k < len(code) and code[k] == "(":
                k = _match_paren(code, k) + 1
            skip_to = k
            eq = k
            while eq < len(code) and code[eq] in " \t":
                eq += 1
            endless = code.startswith("=", eq) and not code.startswith(("==", "=~", "=>"), eq)
            b = {"kind": "def", "name": dm.group(2), "on": dm.group(1) or "", "pos": pos, "params": (params_at, k),
                 "name_span": (dm.start(2), dm.end(2))}
            if endless:
                b["a"] = line
                e = code.find("\n", k)
                b["b"] = line
                b["endless"] = eq + 1  # where the body expression starts
                b["end_pos"] = len(code) if e < 0 else e
                b["parent"] = stack[-1]["i"] if stack else -1
                b["i"] = len(out)
                out.append(b)
                continue
        elif kw == "class":
            sm = _SCLASS.match(code, pos)
            cm = None if sm else _CLASS.match(code, pos)
            if sm:
                b = {"kind": "sclass"}
            elif cm:
                b = {"kind": "class", "name": cm.group(1), "base": cm.group(2) or ""}
            else:
                b = {"kind": "other"}
        elif kw == "module":
            mm = _MODULE.match(code, pos)
            b = {"kind": "module", "name": mm.group(1), "base": ""} if mm else {"kind": "other"}
        elif kw in ("if", "unless", "while", "until"):
            if not _opens(code, pos):
                continue
            if kw in ("while", "until"):
                loop_do = line
            b = {"kind": "other"}
        elif kw == "for":
            loop_do = line
            b = {"kind": "other"}
        elif kw == "do":
            if loop_do == line:
                loop_do = -1
                continue
            b = _do_block(code, pos)
        else:  # begin, case
            b = {"kind": "other"}
        b["a"] = line
        b["pos"] = b.get("pos", pos)
        b["parent"] = stack[-1]["i"] if stack else -1
        b["i"] = len(out)
        b["b"] = None
        out.append(b)
        stack.append(b)
    last = line_of(len(code))
    for b in out:
        if b["b"] is None:  # unbalanced (a construct we misread): to the end of the file
            b["b"] = last
            b["end_pos"] = len(code)
    return out


def _blockless_classes(blocks: list[dict], lines: list[str]) -> None:
    """`Generator = Class.new(Plugin)` (no block): a class all the same, as a
    one-line block, so jekyll's plugins inherit Plugin's class methods."""
    for i, ln in enumerate(lines):
        m = _CLASS_NEW.match(ln)
        if not m or re.search(r"\bdo\b|\{", ln[m.end():]):
            continue
        around = [b for b in blocks if b["a"] <= i <= b["b"] and b.get("end_pos") is not None]
        parent = max(around, key=lambda b: (b["a"], -b["b"]))["i"] if around else -1
        base = m.group(3) or ("" if m.group(2) == "Class" else m.group(2))
        blocks.append({"kind": "class", "name": m.group(1), "base": base, "a": i, "b": i, "pos": 0,
                       "parent": parent, "i": len(blocks), "end_pos": None})


def _do_block(code: str, pos: int) -> dict:
    head = _stmt_prefix(code, pos)
    a = code.rfind("\n", 0, pos)
    for _ in range(3):  # `should "a, " \` / `"b" do`: a head over several lines
        prev = code.rfind("\n", 0, a)
        if a <= 0 or not code[prev + 1:a].rstrip().endswith(("\\", ",")):
            break
        head = code[prev + 1:a].rstrip().rstrip("\\") + " " + head.lstrip()
        a = prev
    if _TEST_HEAD.match(head):
        return {"kind": "test"}
    dm = _DEFINE_METHOD.search(head)
    if dm:
        return {"kind": "def", "name": dm.group(1), "on": "", "params": None, "name_span": None}
    cm = _CLASS_NEW.match(head)
    if cm:
        base = cm.group(3) or ("" if cm.group(2) == "Class" else cm.group(2))
        return {"kind": "class", "name": cm.group(1), "base": base}
    am = _ANON_CLASS.search(head)
    if am:  # `Class.new(Base) do def call(env) ... end end`: a class of its own, not the enclosing one's methods
        return {"kind": "class", "name": f"{am.group(1)}.new@{code.count(chr(10), 0, pos) + 1}", "base": am.group(2) or ""}
    return {"kind": "do", "setup": bool(re.match(r"\s*(?:setup|before)\b", head))}


# --- the parse ------------------------------------------------------------------

def parse(path: str, rel: str, text: str, res_of=None) -> list[Module]:
    code, strings = blank(text, "ruby")
    raw, lines = text.split("\n"), code.split("\n")
    starts = [0]
    for ln in lines[:-1]:
        starts.append(starts[-1] + len(ln) + 1)

    def line_of(k: int) -> int:
        return bisect.bisect_right(starts, k) - 1

    fm = Module(_modname(rel), rel, len(raw), raw, lines)
    _requires(fm, rel, lines, raw)
    blocks = _blocks(code, line_of)
    _blockless_classes(blocks, lines)
    first_str: dict[int, str] = {}
    for ln, st in strings:
        first_str.setdefault(ln - 1, st)

    # classes and modules: their full names, from the enclosing ones
    mods: dict[str, Module] = {}
    home: dict[int, Module] = {}  # block index -> the module its methods go to
    for b in blocks:
        p = b["parent"]
        outer = home.get(p, fm) if p >= 0 else fm
        if b["kind"] in ("class", "module"):
            written = b["name"]
            enclosing = outer.decls[0] if outer is not fm else ""
            full = written[2:] if written.startswith("::") else f"{enclosing}::{written}" if enclosing else written
            mname = f"{fm.name}.{full}"
            cm = mods.get(mname)
            if cm is None:
                cm = Module(mname, rel, len(raw), raw, lines, owner=fm.name, decls=[full], kind="class")
                mods[mname] = cm
            if b["base"]:
                cm.bases.append(f"S{enclosing}\0{b['base']}")
            if "::" in written.lstrip(":") and "C\0" not in cm.bases:
                cm.bases.append("C\0")  # `class Rack::Builder`: reopening a constant from elsewhere
            home[b["i"]] = cm
        elif p >= 0:
            home[b["i"]] = outer
    # each line's owner: the innermost def / test block, else the class body
    owner_blk = [-1] * len(lines)
    body_blk = [-1] * len(lines)  # the innermost block of any kind: `include M` counts in a class body only
    for b in blocks:
        for i in range(b["a"], b["b"] + 1):
            body_blk[i] = b["i"]
            if b["kind"] in ("class", "module", "def", "test"):
                owner_blk[i] = b["i"]
    test_file = _test_file(rel)
    funcs: dict[tuple[str, str], Func] = {}
    tops: dict[str, Func] = {}
    func_of_blk: dict[int, Func] = {}
    private_from: dict[str, list[tuple[int, str]]] = {}
    for b in blocks:
        if b["kind"] in ("class", "module"):
            cm = home[b["i"]]
            for i in range(b["a"] + 1, b["b"]):
                if body_blk[i] != b["i"]:
                    continue
                pm = _PRIVATE.match(lines[i])
                if pm:
                    private_from.setdefault(cm.name, []).append((i, pm.group(1)))
                mx = _MIXIN.match(lines[i])
                if mx:
                    for c in re.split(r"\s*,\s*", mx.group(2)):
                        if c != "self":
                            cm.bases.append(f"M{cm.decls[0]}\0{c}")
    for i, ln in enumerate(lines):
        mx = _MIXIN.match(ln) if body_blk[i] == -1 else None
        if mx and mx.group(1) == "include":
            for c in re.split(r"\s*,\s*", mx.group(2)):
                fm.bases.append(f"G\0{c}")  # `include Jekyll` at the top: in Object, so everywhere
    for b in blocks:
        if b["kind"] not in ("def", "test"):
            continue
        mod = home.get(b["parent"], fm) if b["parent"] >= 0 else fm
        a, z = b["a"], b["b"]
        if b["kind"] == "test":
            label = first_str.get(a, f"line {a + 1}")
            name = f"test {label}"
            kind = "test"
        else:
            name = b["name"]
            kind = "test" if test_file and name.startswith("test_") and mod is not fm else "func"
            if kind == "func" and not b["on"]:
                marks = [w for i, w in private_from.get(mod.name, []) if i < a]
                if marks and marks[-1] != "public" or re.match(r"^\s*private\s+def\b", lines[a]):
                    kind = "private"
        f = funcs.get((mod.name, name))
        if f is None:
            f = Func(mod.name, name, rel, a + 1, z + 1, "\n".join(lines[a:z + 1]), kind=kind)
            funcs[(mod.name, name)] = f
            (mods.get(mod.name) or fm).funcs.append(f)
        elif z - a > f.end - f.start:
            f.start, f.end, f.code = a + 1, z + 1, "\n".join(lines[a:z + 1])
        func_of_blk[b["i"]] = f
    for m in [fm, *mods.values()]:
        top = Func(m.name, "<top>", rel, 1, len(lines), "", kind="top")
        tops[m.name] = top
        m.funcs.append(top)
    _attrs(fm, mods, blocks, home, body_blk, lines, funcs)
    _lets(fm, mods, blocks, home, code, starts, lines, funcs)

    # calls, locals and bindings, function by function
    ends = {b["end_pos"]: b for b in blocks if b["kind"] == "do"}
    owned: dict[int, list[int]] = {}
    for i, ob in enumerate(owner_blk):
        owned.setdefault(ob, []).append(i)
    for ob, idx in owned.items():
        b = blocks[ob] if ob >= 0 else None
        if b is None:
            f = tops[fm.name]
        elif b["kind"] in ("class", "module"):
            f = tops[home[ob].name]
        else:
            f = func_of_blk[ob]
        if f.kind == "top":
            mine = set(idx)
            f.code = "\n".join(lines[i] if i in mine else "" for i in range(len(lines)))
        _collect(f, b, code, starts, idx, lines, raw, ends)
    # globals reach nothing in the project: `$stdout.puts`
    for g in set(re.findall(r"(?<![\w$])\$([A-Za-z_]\w*)", code)):
        fm.aliases["$" + g] = "ext:"
    _class_fields(mods, [*func_of_blk.values(), *tops.values()])
    _setup_fields(blocks, func_of_blk, code, starts)
    for cm in mods.values():
        # names in a class resolve through its file: share its requires
        cm.aliases, cm.uses, cm.reexports = fm.aliases, fm.uses, fm.reexports
    return [fm] + list(mods.values())


def _requires(fm: Module, rel: str, lines: list[str], raw: list[str]) -> None:
    """`require "x/y"`, `require_relative "z"`, `autoload :K, "x/k"`: kept as
    "rb:<dir>\\0<spec>\\0<relative>" for link() to find in the project."""
    d = os.path.dirname(rel)
    for i, ln in enumerate(lines):
        if "require" not in ln and "autoload" not in ln and "load" not in ln:
            continue
        m = _REQUIRE.match(raw[i])
        if m:
            fm.uses.append(f"rb:{d}\0{m.group(2)}\0{'1' if m.group(1) == 'require_relative' else ''}")
            continue
        m = _AUTOLOAD.match(raw[i])
        if m:
            fm.uses.append(f"rb:{d}\0{m.group(1)}\0")


def _attr_names(lines: list[str], i: int, head_end: int) -> tuple[list[str], int]:
    """The symbols an `attr_reader :a, :b,` names (it may go on over lines)."""
    text, j = lines[i][head_end:], i
    while text.rstrip().endswith(",") and j + 1 < len(lines):
        j += 1
        text += " " + lines[j]
    return re.findall(r":(\w+)", text), j


def _attrs(fm, mods, blocks, home, body_blk, lines, funcs) -> None:
    """attr_reader / attr_accessor are methods returning their field;
    def_delegators / delegate are methods calling on through a field."""
    for b in blocks:
        if b["kind"] not in ("class", "module", "sclass") or b["i"] not in home:
            continue  # (`class << self; attr_accessor :lsi`: the class's own)
        cm = home[b["i"]]
        for i in range(b["a"] + 1, b["b"]):
            if body_blk[i] != b["i"]:
                continue
            ln = lines[i]
            am = _ATTR.match(ln)
            if am:
                names, _ = _attr_names(lines, i, am.end())
                for nm in names:
                    if (cm.name, nm) not in funcs:
                        f = Func(cm.name, nm, cm.file, i + 1, i + 1, ln, returns="expr:@" + nm)
                        funcs[(cm.name, nm)] = f
                        cm.funcs.append(f)
                continue
            dm = _DELEGATORS.match(ln)
            rm = None if dm else _RAILS_DELEGATE.match(ln)
            if dm or rm:
                via = (dm or rm).group(1) if dm else rm.group(2)
                names = re.findall(r":(\w+[?!]?)", dm.group(2) if dm else rm.group(1))
                if dm and ln.lstrip().startswith("def_delegator "):
                    names = names[:1]  # def_delegator :@x, :meth, :alias: one method
                seg = via if via.startswith("@") else via + "()"
                for nm in names:
                    if (cm.name, nm) in funcs:
                        continue
                    f = Func(cm.name, nm, cm.file, i + 1, i + 1, ln)
                    f.calls.append(Call(nm, "." if seg.startswith("@") else via, i + 1, (seg,), max(0, ln.find(nm))))
                    funcs[(cm.name, nm)] = f
                    cm.funcs.append(f)


def _lets(fm, mods, blocks, home, code, starts, lines, funcs) -> None:
    """RSpec `let(:site) { fixture_site }` / `subject { ... }` define a
    method of the example group: a bare `site` calls it."""
    for i, ln in enumerate(lines):
        m = _LET.match(ln)
        if not m:
            continue
        name = m.group(1) or "subject"
        inner = [b for b in blocks if b["a"] <= i <= b["b"] and b["kind"] != "do" or b["a"] < i <= b["b"]]
        if any(b["kind"] in ("def", "test") for b in inner):
            continue
        cls = [b for b in inner if b["kind"] in ("class", "module")]
        mod = home[max(cls, key=lambda b: b["a"])["i"]] if cls else fm
        if (mod.name, name) in funcs:
            continue
        f = Func(mod.name, name, mod.file, i + 1, i + 1, ln)
        ch = expression(code, starts[i] + m.end(), lambda n, p: False) if m.group(2) == "{" else ()
        if ch:
            f.returns = "expr:" + "\x1f".join(ch)
        funcs[(mod.name, name)] = f
        (mods.get(mod.name) or fm).funcs.append(f)


# --- calls ----------------------------------------------------------------------

def _params(code: str, span) -> list[str]:
    """Parameter names of a def head `(a, b = 1, *c, d:, &e)`."""
    if not span:
        return []
    a, z = span
    text = code[a:z]
    if text.startswith("("):
        text = text[1:-1]
    else:  # `def x a, b` (no parentheses): to the end of the line
        e = code.find("\n", a)
        text = code[a:e if e >= 0 else len(code)]
    out, depth, cur = [], 0, ""
    for ch in text + ",":
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        if ch == "," and depth == 0:
            m = re.match(r"\s*[*&]{0,2}([a-z_]\w*)", cur)
            if m:
                out.append(m.group(1))
            cur = ""
        else:
            cur += ch
    return out


def _locals(f: Func, b, code: str, spans: list[tuple[int, int]]) -> dict[str, int]:
    """Local name -> the position it is first assigned at (-1: a parameter)."""
    loc: dict[str, int] = {}
    if b is not None and b["kind"] == "def":
        for p in _params(code, b.get("params")):
            loc[p] = -1
    for a, z in spans:
        seg = code[a:z]
        for rx in (_BLOCK_PARAMS, _FOR_VAR):
            for m in rx.finditer(seg):
                for nm in re.findall(r"[a-z_]\w*", m.group(1)):
                    loc.setdefault(nm, a + m.start())
        for m in _RESCUE_VAR.finditer(seg):
            loc.setdefault(m.group(1), a + m.start(1))
        for m in _MULTI.finditer(seg):
            for nm in re.findall(r"(?<![@\w])[a-z_]\w*", m.group(1)):  # `all, @path = ...`: all is a local
                loc.setdefault(nm, a + m.start(1))
        for m in _ASSIGN.finditer(seg):
            loc.setdefault(m.group(1), a + m.start(1))
    return loc


def _ident_back(code: str, i: int) -> int:
    k = i
    while k > 0 and (code[k - 1].isalnum() or code[k - 1] == "_"):
        k -= 1
    return k


def _back_ws(code: str, i: int) -> int:
    while i > 0 and code[i - 1] in " \t\r\n":
        i -= 1
    return i


def _match_back(code: str, i: int, close: str, open_: str) -> int:
    depth, k = 0, i - 1
    while k >= 0:
        ch = code[k]
        if ch in ")]}":
            depth += 1
        elif ch in "([{":
            depth -= 1
            if depth == 0:
                return k if ch == open_ else -1
        k -= 1
    return -1


def _segment_back(code: str, i: int) -> tuple[str, int]:
    """The receiver segment that ends at i: ("name" | "@x" | "A::B" | "x()" |
    "[]" | "(...)", where it starts); ("", i) when there is none."""
    if i == 0:
        return "", i
    ch = code[i - 1]
    if ch in "\"'`":
        return "String", i - 1  # a literal's methods are Ruby's: `"a,b".split`
    if ch == "/":
        return "Regexp", i - 1
    if ch == ")":
        k = _match_back(code, i, ")", "(")
        if k < 0:
            return "(...)", i
        j = k
        s = _ident_back(code, j)
        if s > 0 and code[s - 1] == "%" and j - s <= 1:  # %w(a b), %(text), %r{re}
            return _PCT_TYPE.get(code[s:j], "String"), s - 1
        if s == j:
            return "(...)", k
        return code[s:j] + "()", s
    if ch == "]":
        k = _match_back(code, i, "]", "[")
        if k < 0:
            return "(...)", i
        if k > 1 and code[k - 2] == "%" and code[k - 1] in _PCT_TYPE:
            return _PCT_TYPE[code[k - 1]], k - 2
        if k == 0 or not (code[k - 1].isalnum() or code[k - 1] in "_)]?!"):
            return "Array", k  # `[a, b].each`
        return "[]", k
    if ch in "?!" and i >= 2 and (code[i - 2].isalnum() or code[i - 2] == "_"):
        s = _ident_back(code, i - 1)
        return code[s:i] + "()", s
    if ch.isalnum() or ch == "_":
        s = _ident_back(code, i)
        name = code[s:i]
        if name[0].isdigit():
            return "Integer", s
        if name in _LITERAL_WORDS:
            return _LITERAL_WORDS[name], s
        if s >= 1 and code[s - 1] == ":" and not (s >= 2 and code[s - 2] == ":"):
            return "Symbol", s - 1
        if s >= 2 and code[s - 2:s] == "@@":
            return "@@" + name, s - 2
        if s >= 1 and code[s - 1] == "@":
            return "@" + name, s - 1
        if s >= 1 and code[s - 1] == "$":
            return "$" + name, s - 1
        if name[0].isupper():
            while s >= 2 and code[s - 2:s] == "::":
                t = _ident_back(code, s - 2)
                if t < s - 2 and code[t].isupper():
                    name, s = code[t:s - 2] + "::" + name, t
                else:
                    s -= 2 if t == s - 2 else 0
                    break
        return name, s
    return "(...)", i


def receiver(code: str, pos: int, is_local) -> tuple[str, ...]:
    """The chain a method call at pos is made on: `@site.config.fetch` ->
    ("@site", "config()"); every `.name` is a call (Ruby needs no
    parentheses), a bare name is a local or a call (is_local says), `A::B`
    is one segment, `x.class` is x's class (one module with its singleton
    methods)."""
    segs: list[str] = []
    i = _back_ws(code, pos)
    while len(segs) < 8:
        if i >= 1 and code[i - 1] == "." and not (i >= 2 and code[i - 2] == "."):
            i -= 1
            if i >= 1 and code[i - 1] == "&":
                i -= 1
        elif i >= 2 and code[i - 2:i] == "::":
            i -= 2
        else:
            break
        i = _back_ws(code, i)
        seg, s = _segment_back(code, i)
        if not seg:
            break
        if seg == "[]":
            segs.append("[]")
            seg, s = _segment_back(code, s)
            if not seg or seg == "(...)":
                segs.append("(...)")
                break
        if seg == "class" and s > 0 and code[s - 1] == ".":
            seg = "class()"  # `self.class.x`: a method, not the keyword
        if seg.rstrip("()") in _KEYWORDS and seg not in ("self", "class()"):
            segs.append("(...)")  # `end.map`, `return x.y`: not a receiver we can name
            break
        segs.append(seg)
        i = s
        if seg == "(...)":
            break
    segs.reverse()
    return _normalize(segs, is_local, pos)


def _normalize(segs: list[str], is_local, pos: int) -> tuple[str, ...]:
    out: list[str] = []
    for k, s in enumerate(segs):
        if (s[:1].islower() or s[:1] == "_") and s != "self":
            bare = s.endswith("()") is False
            if bare and (k > 0 or not is_local(s, pos)):
                s += "()"
        if s == "class()" and out:
            continue  # `self.class.x`: the class's singleton method, same module here
        if s in _SAME and out:
            continue  # `x.tap { }`, `x.dup`: x itself
        out.append(s)
    return tuple(out)


_SAME = {"tap()", "dup()", "clone()", "freeze()", "itself()"}  # `x.tap { }`, `x.dup`: x itself
_DO_END = re.compile(r"\bdo\s*(?:\|[^|]*\|)?\s*$")
_EXPR_HEAD = re.compile(rf"(@@?\w+|\$\w+|{_CONST}|{_ID}[?!]?)")
_EXPR_NAME = re.compile(rf"\s*({_ID}[?!]?)")
_NUMBER = re.compile(r"[\d_]+(\.\d+)?")
_SYM_LIT = re.compile(r":\w+[?!=]?")
_PCT_LIT = re.compile(r"%([qQwWiIrsx]?)([(\[{<|!/^])")


def _literal(code: str, i: int) -> tuple[str, int] | None:
    """A literal starting at i (contents blanked): its class and its end."""
    ch = code[i:i + 1]
    if not ch:
        return None
    if ch in "\"'`/":
        e = code.find(ch, i + 1)
        return ("Regexp" if ch == "/" else "String", e + 1) if e > 0 else None
    if ch in "[{":
        return ("Array" if ch == "[" else "Hash", _match_paren(code, i) + 1)
    if ch.isdigit():
        m = _NUMBER.match(code, i)
        return ("Float" if m.group(1) else "Integer", m.end())
    if ch == ":" and re.match(r"[A-Za-z_]", code[i + 1:i + 2]):
        m = _SYM_LIT.match(code, i)
        return ("Symbol", m.end())
    m = _PCT_LIT.match(code, i)
    if m:
        close = {"(": ")", "[": "]", "{": "}", "<": ">"}.get(m.group(2), m.group(2))
        e = code.find(close, m.end()) if close in (m.group(2), ">") else _match_paren(code, m.end() - 1)
        return (_PCT_TYPE.get(m.group(1), "String"), e + 1) if e > 0 else None
    return None


def expression(code: str, pos: int, is_local) -> tuple[str, ...]:
    """The chain `x = <expr>` builds when <expr> is a plain one: `Foo.new(a)`
    -> ("Foo", "new()"); `@site.reader` -> ("@site", "reader()"). () when it
    is anything else (arithmetic, a literal, a conditional)."""
    n = len(code)
    i = pos
    while i < n and code[i] in " \t":
        i += 1
    segs: list[str] = []
    lit = _literal(code, i)
    if lit:
        tok, i = lit
    else:
        m = _EXPR_HEAD.match(code, i)
        if not m:
            return ()
        tok = m.group(1).lstrip(":") if m.group(1).startswith("::") else m.group(1)
        if tok in _LITERAL_WORDS:
            tok = _LITERAL_WORDS[tok]
        elif tok in _KEYWORDS and tok != "self":
            return ()
        i = m.end()
        if i < n and code[i] == "(":
            i = _match_paren(code, i) + 1
            tok = tok if tok.endswith("()") else tok + "()"
        elif i < n and code[i] == "[" and tok[:1].isupper():
            segs.append(tok)  # `Configuration[{...}]`: Hash.[] on a class (see link)
            i = _match_paren(code, i) + 1
            tok = "[]()"
    segs.append(tok)
    while len(segs) < 8:
        if i < n and code[i] == "[" and segs[-1] != "[]" and segs[-1] != "[]()":
            close = _match_paren(code, i)  # `@site.collections["x"]`: an element
            if close < 0 or "\n" in code[i:close]:
                return ()
            segs.append("[]")
            i = close + 1
            continue
        j = i
        while j < n and code[j] in " \t":
            j += 1
        nl = j
        while nl < n and code[nl] in " \t\r\n":
            nl += 1
        if code.startswith("&.", nl) or (code.startswith(".", nl) and not code.startswith("..", nl)):
            i = nl + (2 if code[nl] == "&" else 1)
            mm = _EXPR_NAME.match(code, i)
            if not mm:
                return ()
            i = mm.end()
            name = mm.group(1)
            if i < n and code[i] == "(":
                i = _match_paren(code, i) + 1
            k = i
            while k < n and code[k] in " \t":
                k += 1
            if k < n and code[k] == "{":  # a brace block: the method's result
                i = _match_paren(code, k) + 1
            segs.append(name + "()")
            continue
        rest = code[j:code.find("\n", j) if code.find("\n", j) >= 0 else n].rstrip()
        if not rest or rest[0] in ";)],}" or re.match(r"(?:if|unless|rescue|do|while|until|then)\b", rest):
            break
        if segs[-1].endswith("()") or segs[-1][:1].islower():
            # `Foo.new a, b` / `render x`: arguments without parentheses
            if j > i and re.match(r"[\w@:\"'\[{(*&$-]", rest) and not re.match(r"(?:and|or|\?)\b", rest) \
                    and not rest.startswith(("-=", "--", "- ")):
                break
        return ()
    return _normalize(segs, is_local, pos)


def _collect(f: Func, b, code: str, starts: list[int], idx: list[int], lines: list[str], raw: list[str], ends: dict) -> None:
    """The calls (and locals' bindings) on the lines f owns."""
    spans = [(starts[i], starts[i] + len(lines[i])) for i in idx]
    loc = _locals(f, b, code, spans)

    def is_local(name: str, pos: int) -> bool:
        p = loc.get(name)
        return p is not None and p <= pos  # (`output, warnings = ...`: the target itself is no call)

    skip: tuple[int, int] = (-1, -1)
    if b is not None and b["kind"] == "def" and b.get("name_span"):
        skip = (b["pos"], b["name_span"][1])  # `def site.process`: neither name is a call
    for i in idx:
        a = starts[i]
        ln = lines[i]
        if not ln.strip():
            continue
        lead = ln.lstrip()
        am_ = _ALIAS.match(lead)
        if am_:  # `alias stop! quit!`: two method names, not calls
            f.refs.update(am_.groups())
            continue
        for m in _WORD.finditer(ln):
            name = m.group(1)
            p = a + m.start(1)
            if skip[0] <= p < skip[1]:
                continue
            e = a + m.end(1)
            nxt = code[e:e + 2]
            if name.endswith("!") and nxt[:1] == "=":
                name, e = name[:-1], e - 1  # `x!= y`
                nxt = code[e:e + 2]
            prev = code[p - 1] if p else ""
            prev2 = code[p - 2:p]
            if nxt[:1] == ":" and nxt != "::":
                continue  # a hash key / keyword argument `name:`
            if prev == ":" and prev2 != "::":
                continue  # a symbol
            if prev in "@$" or name[0].isupper() or name[0].isdigit():
                continue
            after = code[e:e + 4].lstrip(" \t")
            assigns = after.startswith("=") and not after.startswith(("==", "=~", "=>"))
            col = m.start(1)
            if (prev == "." and prev2 != "..") or prev2 == "::":  # (`a..b`: b is a plain name)
                if not assigns:  # (`obj.x = v`: a setter)
                    _add(f, name, receiver(code, p, is_local), i + 1, col)
                continue
            if name in _KEYWORDS:
                if name == "super" and b is not None and b["kind"] == "def":
                    f.calls.append(Call(b["name"], "super", i + 1, (), col))
                continue
            if assigns:
                continue
            if name in _DECLARE:
                continue
            # (`context "x" do`, `setup do`: the test framework's, or the project's own DSL
            # (sinatra's test/contest.rb): recorded, and resolved like any bare call)
            if code[e:e + 1] != "(" and is_local(name, p):
                continue
            f.calls.append(Call(name, "", i + 1, (), col))
        for m in _SYMBOL.finditer(ln):
            f.refs.add(m.group(1).lstrip("@"))
        for cb in _CONST_BLOCK.finditer(ln):
            const, args = cb.group(1).lstrip(":"), cb.group(2) or cb.group(3) or ""
            if const.split("::")[0] not in _STDLIB_YIELDS and re.search(r"(?<![:\w@$])[a-z_]\w*", args):
                continue  # it was handed a value of ours: what it yields may be ours
            for nm in re.findall(r"[a-z_]\w*", cb.group(4)):
                f.bound.setdefault(nm, "lib:" + const)  # link() keeps it if the constant is a library's
        for im_ in _ITER_BLOCK.finditer(ln):  # `docs.sort.each do |doc|`: doc is an element of docs
            src = list(expression(code, a + im_.start(1), is_local))
            if not src or src[-1][:-2] not in _ITERS:
                continue
            src.pop()  # the iterator itself
            while len(src) > 1 and src[-1][:-2] in _SAME_ELEMENTS:
                src.pop()  # `docs.sort.each`: the same elements
            if not src or src == ["self"]:
                continue
            if src[-1].endswith("()"):
                src[-1] = src[-1][:-2]  # (a reader: `docs` is `@docs`, read for its `docs[]`)
            nm = re.findall(r"[a-z_]\w*", im_.group(2))[:1]
            if nm:
                spec = "elem:" + "\x1f".join(src)
                held = f.bound.get(nm[0])
                if held is None:
                    f.bound[nm[0]] = spec
                elif held != spec and held.startswith("elem:"):
                    f.bound[nm[0]] = "lam:?"  # (one name, two collections)
        rb_ = _RESCUE_BIND.search(ln)
        if rb_:
            classes = re.split(r"\s*,\s*", rb_.group(1)) if rb_.group(1) else ["StandardError"]
            if len(classes) == 1:  # `rescue Foo => e`: e is a Foo (a bare `rescue`, a StandardError)
                f.bound.setdefault(rb_.group(2), f"expr:{classes[0]}\x1fnew()")
        im = _IVAR_SET.match(ln, len(ln) - len(lead))
        if im:
            ch = expression(code, a + im.end(), is_local)
            if ch and not (ch[0][:1].islower() and not ch[0].endswith("()") and ch[0] not in f.bound):
                f.bound.setdefault(im.group(1), "expr:" + "\x1f".join(ch))
        if f.kind != "top":
            am = _ASSIGN.match(ln, len(ln) - len(lead))
            if am and not re.match(r"\s*(?:\|\||&&|\*\*|<<|>>|[-+*/%|&^])=", ln[am.end(1):]):
                ch = expression(code, a + am.end(), is_local)
                if ch and ch[0] != am.group(1) + "()":
                    f.bound.setdefault(am.group(1), "expr:" + "\x1f".join(ch))
    if b is not None and b["kind"] == "def" and f.name != "<top>":
        _declared(f, b, code, raw, is_local, ends)


def _add(f: Func, name: str, chain: tuple, line: int, col: int) -> None:
    if chain and chain[-1] in ("self",):
        q = "self"
    elif chain and chain[-1][:1].isupper():
        q = chain[-1].replace("::", ".")
    elif chain and re.match(r"^\w+$", chain[-1]):
        q = chain[-1]
    else:
        q = "."
    f.calls.append(Call(name, q, line, chain, col))


def _yard_type(text: str) -> str:
    parts = [p.strip() for p in text.split(",") if p.strip() not in ("nil", "")]
    if len(parts) != 1:
        return ""
    t = type_name(parts[0].replace("::", "\0")).replace("\0", "::")
    if parts[0].strip() in ("self", "Self"):
        return "Self"
    if not t or t in ("Boolean", "void", "Object") or not t[:1].isupper():
        return ""
    return t


def _declared(f: Func, b, code: str, raw: list[str], is_local, ends: dict) -> None:
    """A method's return type and its parameters' types, from what the code
    says: YARD `@return [Foo]` / `@param x [Foo]`, a Sorbet `sig`, and the
    last expression of the body (`Foo.new(...)`, `self`, `@field`)."""
    a = b["a"]
    doc = []
    j = a - 1
    while j >= 0 and (raw[j].lstrip().startswith("#") or raw[j].lstrip().startswith("sig")
                      or raw[j].strip() in ("end", "}") and doc and doc[-1].lstrip().startswith("sig")):
        doc.append(raw[j])
        j -= 1
    for d in doc:
        rm = _YARD_RET.match(d)
        if rm and not f.returns:
            t = _yard_type(rm.group(1))
            if t:
                f.returns = t if t == "Self" else f"expr:{t}\x1fnew()"
        pm = _YARD_PARAM.match(d) or _YARD_PARAM2.match(d)
        if pm:
            nm, ty = (pm.group(1), pm.group(2)) if pm.re is _YARD_PARAM else (pm.group(2), pm.group(1))
            t = _yard_type(ty)
            if t and t != "Self":
                f.bound.setdefault(nm, f"expr:{t}\x1fnew()")
    sig = " ".join(d for d in reversed(doc) if not d.lstrip().startswith("#"))
    if sig:
        sm = _SIG_RET.search(sig)
        if sm and not f.returns and sm.group(1)[:1].isupper():
            f.returns = f"expr:{sm.group(1)}\x1fnew()"
        for nm, ty in _SIG_PARAM.findall(sig.split(".returns")[0]):
            f.bound.setdefault(nm, f"expr:{ty}\x1fnew()")
    if f.returns:
        return
    if b.get("endless"):
        ch = expression(code, b["endless"], is_local)
    else:
        end_pos = b.get("end_pos", 0)
        k = code.rfind("\n", 0, end_pos)
        s = code.rfind("\n", 0, k) + 1 if k > 0 else 0
        # the last statement before `end`: a line of its own, not a nested block's end
        while k > 0 and not code[s:k].strip() and s > code.find("\n", b["pos"]):
            k = s - 1
            s = code.rfind("\n", 0, k) + 1
        body_start = code.find("\n", b["pos"])
        # a statement over several lines (`Foo.new(a,\n b).tap(&:read)`): from its first line
        while s > body_start + 1 and code[s:k].count(")") > code[s:k].count("("):
            s = code.rfind("\n", 0, s - 1) + 1
        last = code[s:k].strip() if k > 0 and s > body_start else ""
        blk = ends.get(code.find("end", s)) if last == "end" else None
        if blk is not None:  # `Foo.new(x).tap do |f| ... end`: the value the block hangs off
            s = code.rfind("\n", 0, blk["pos"]) + 1
            last = code[s:blk["pos"]].strip()
        if not last or re.match(r"(?:end|else|elsif|when|in|rescue|ensure|then|\}|\])\b", last) or last.endswith(("do", "|")):
            return
        st = code.find(last, s)
        if last.startswith("return "):
            st += 7
        am = re.match(r"@{0,2}\w+\s*(?:\|\|)?=(?![=~>])\s*|[@\w.]+\[[^\]\n]*\]\s*\|\|=\s*", last)
        if am:
            st += am.end()  # `@x ||= Foo.new(...)`, `cache[k] ||= build(k)`: the value assigned
        ch = expression(code, st, is_local)
    if not ch:
        return
    if ch == ("self",) or ch == ("new()",):
        f.returns = "Self"
    elif not (ch[0][:1].islower() and not ch[0].endswith("()") and ch[0] not in f.bound):
        f.returns = "expr:" + "\x1f".join(ch)


def _setup_fields(blocks, func_of_blk, code: str, starts: list[int]) -> None:
    """A test's fields from the `setup do` / `before do` blocks of the
    contexts around it (the innermost first): each context sets up its own."""
    setups: dict[int, list[dict]] = {}
    for b in blocks:
        if b.get("setup") and b["parent"] >= 0:
            setups.setdefault(b["parent"], []).append(b)
    if not setups:
        return
    for bi, f in func_of_blk.items():
        if f.kind != "test":
            continue
        p = blocks[bi]["parent"]
        while p >= 0:
            for s in setups.get(p, ()):
                for m in _IVAR_SET.finditer(code, starts[s["a"]], starts[min(s["b"], len(starts) - 1)]):
                    ch = expression(code, m.end(), lambda n, _p: False)
                    if ch and not (ch[0][:1].islower() and not ch[0].endswith("()")):
                        f.bound.setdefault(m.group(1), "expr:" + "\x1f".join(ch))
            p = blocks[p]["parent"]


def _class_fields(mods: dict, funcs) -> None:
    """`@x = Foo.new` in a class's methods (or a `setup do` block in its
    body): the field's type, for every method. Several values (each test
    context's own `setup`) are kept as "alt:" for link() to check that they
    all build the same class; one it can't read says nothing."""
    seen: dict[str, dict[str, set]] = {}
    for f in funcs:
        m = mods.get(f.module)
        if m is None:
            continue
        accessors = {g.name for g in m.funcs if g.returns == f"expr:@{g.name}"}
        for k, v in _ivar_sets(f, accessors):
            seen.setdefault(m.name, {}).setdefault(k, set()).add(v)
        for k, v in _pushes(f, m):
            seen.setdefault(m.name, {}).setdefault(k, set()).add(v)
    for mn, fields in seen.items():
        for k, vs in fields.items():
            if len(vs) > 1:
                vs = vs - {"expr:NilClass"}  # `@theme = nil` here, `@theme = Theme.new(...)` there: a Theme
            if "?" not in vs:
                mods[mn].bound.setdefault(k, next(iter(vs)) if len(vs) == 1 else "alt:" + "\x1e".join(sorted(vs)))


def _ivar_sets(f: Func, accessors: set):
    """(field, value) for each `@x = <expr>` in f, and each `self.x = <expr>`
    through an attr_accessor (the same field)."""
    params = _params_of(f)
    if f.name == "initialize":  # `@site = relations[:site]`: an option the callers pass by name
        for m in re.finditer(r"(?<![\w@])(@\w+)\s*=\s*([a-z_]\w*)\[:(\w+)\]\s*$", f.code, re.M):
            if m.group(2) in params:
                yield m.group(1), f"expr:{m.group(2)}\x1f[:{m.group(3)}]"
    sets = [(m.group(1), m.end()) for m in _IVAR_SET.finditer(f.code)
            if not re.match(r"\s*[a-z_]\w*\[:\w+\]\s*$", f.code[m.end():f.code.find("\n", m.end())])]
    sets += [("@" + m.group(1), m.end()) for m in _SELF_SET.finditer(f.code) if m.group(1) in accessors]
    for name, at in sets:
        ch = expression(f.code, at, lambda n, p: n in f.bound or n in params)
        if not ch:
            yield name, "?"
            continue
        if ch[0] in f.bound and f.bound[ch[0]].startswith("expr:"):  # `@site = site` with site's type known
            yield name, "expr:" + "\x1f".join((*f.bound[ch[0]][5:].split("\x1f"), *ch[1:]))
        elif ch[0][:1].islower() and not ch[0].endswith("()"):
            # `@site = doc.site`: as written, for the constructor's callers to type (_ctor_params)
            yield name, "expr:" + "\x1f".join(ch) if f.name == "initialize" and ch[0] in params else "?"
        else:
            yield name, "expr:" + "\x1f".join(ch)


_PUSH = re.compile(r"(?<![\w@.:])(@?[a-z_]\w*)\s*(?:<<|\.push\(|\.append\(|\.unshift\()\s*"
                   r"(?:([A-Z][\w:]*)\.new\b|([a-z_]\w*)\s*(?:$|[)\n]|\bif\b|\bunless\b))", re.M)


def _pushes(f: Func, m):
    """("@docs[]", <element>) for each `docs << doc` / `@docs.push(Foo.new)` in
    f: what the collection holds, as `docs[0]`, `docs.first`, `docs.find {}`
    read it. A bare `docs` is the class's own reader of `@docs`."""
    own = {g.name: g for g in m.funcs}
    for pm in _PUSH.finditer(f.code):
        coll, cls, var = pm.groups()
        if cls:
            val = f"expr:{cls}\x1fnew()"
        else:
            held = f.bound.get(var, "")
            if not (held.startswith("expr:") and held[5:6].isupper() and held.endswith("\x1fnew()")):
                yield (coll if coll.startswith("@") else "@" + coll) + "[]", "?"
                continue
            val = held
        if coll.startswith("@"):
            yield coll + "[]", val
        elif coll not in f.bound and coll in own and f"@{coll}" in own[coll].code:
            yield coll + "[]", val
            yield f"@{coll}[]", val
    # `@collections ||= names.each_with_object({}) do |name, hsh| hsh[name] = Collection.new(...)`
    acc = re.search(rf"(@{re.escape(f.name)})\s*\|\|=.*?\.each_with_object\(\s*(?:\{{\s*\}}|\[\s*\]|Hash\.new|\{{\}})\s*\)\s*"
                    r"(?:do|\{)\s*\|[^|,]*,\s*([a-z_]\w*)\s*\|", f.code, re.S)
    for hm in re.finditer(r"(?<![\w@.:])(@?[a-z_]\w*)\[[^\]\n]+\]\s*=\s*([A-Z][\w:]*)\.new\b", f.code):
        coll, cls = hm.groups()
        if coll.startswith("@"):
            yield coll + "[]", f"expr:{cls}\x1fnew()"
        elif acc and coll == acc.group(2):
            yield f.name + "[]", f"expr:{cls}\x1fnew()"
            yield acc.group(1) + "[]", f"expr:{cls}\x1fnew()"


def _params_of(f: Func) -> set[str]:
    m = re.match(rf"\s*def\s+(?:\w+\.)?{re.escape(f.name)}\s*\(([^)]*)\)", f.code)
    return set(re.findall(r"[a-z_]\w*", m.group(1))) if m else set()


# --- linking (core calls this once every file is parsed) ---------------------------

def link(project) -> None:
    """Requires to files, constants to class modules, supertypes to modules,
    then the types of parameters and fields from their callers."""
    rb = [m for m in project.modules.values() if m.file.endswith(EXT)]
    if not rb:
        return
    _link_requires(project, rb)
    r = _link_classes(project, rb)
    _bare_calls_on_self(project, rb)
    _block_forwarders(rb)
    _rewrite_constants(rb, r)
    _infer_types(project, rb, r)


_CONST_ALIAS = re.compile(r"^([ \t]*)([A-Z]\w*)[ \t]*=[ \t]*((?:::)?[A-Z]\w*(?:::[A-Z]\w*)*)[ \t]*(?:#.*)?$")
_SCOPE_OPEN = re.compile(r"^([ \t]*)(?:class|module)[ \t]+((?:::)?[A-Z][\w:]*)")


def _const_aliases(rb: list, r: "_Resolver") -> None:
    """`Stream = Sinatra::Helpers::Stream` in a class body: in that scope, Stream is that class."""
    for m in rb:
        if m.owner:
            continue
        stack: list[tuple[int, str]] = []
        for ln in m.code if isinstance(m.code, list) else []:
            om = _SCOPE_OPEN.match(ln)
            if om:
                ind = len(om.group(1).expandtabs(2))
                while stack and stack[-1][0] >= ind:
                    stack.pop()
                stack.append((ind, om.group(2).lstrip(":")))
                continue
            am = _CONST_ALIAS.match(ln)
            if not am or am.group(3).lstrip(":") == am.group(2):
                continue
            ind = len(am.group(1).expandtabs(2))
            while stack and stack[-1][0] >= ind:
                stack.pop()
            path = "::".join(n for _, n in stack)
            nest = ["::".join(path.split("::")[:k]) for k in range(len(path.split("::")), 0, -1)] if path else []
            r.aliases[f"{path}::{am.group(2)}" if path else am.group(2)] = (nest, am.group(3))


def _link_classes(project, rb: list) -> "_Resolver":
    """Each constant's class (one of its reopenings), its supertypes and
    mixins as modules; the reopenings linked to it both ways."""
    idx: dict[str, list[str]] = {}
    for m in rb:
        if m.owner and m.decls:
            idx.setdefault(m.decls[0], []).append(m.name)
    canon = {full: _canonical(project, full, names) for full, names in idx.items()}
    r = _Resolver(project, idx, canon)
    _const_aliases(rb, r)
    for m in rb:
        if not m.owner:
            r.globals += [w for w in (r.resolve([], b[2:]) for b in m.bases if b.startswith("G\0")) if w]
            m.bases = []
    for full, names in idx.items():
        c = canon[full]
        for mn in names:
            m = project.modules[mn]
            m.kind = "class" if mn == c else "ext"
    # a library class the project only adds to (`class Rack::Builder`,
    # `class Minitest::Test`: every declaration in that reopening form, none
    # with a superclass or an `initialize`): still a supertype test classes
    # inherit the added methods from, but a value of that type is the
    # library's (sinatra's `builder.use` is Rack's `use`, not a test subclass's)
    lib_reopened = {full for full, names in idx.items()
                    if all("C\0" in project.modules[mn].bases and not any(b.startswith("S") for b in project.modules[mn].bases)
                           and f"{mn}.initialize" not in project.funcs for mn in names)}
    for m in rb:
        if m.owner:
            r.bases(m)
    for full, names in idx.items():
        c = project.modules[canon[full]]
        others = [mn for mn in names if mn != c.name]
        c.bases = others + [x for x in c.bases if x not in others]
        for mn in others:
            o = project.modules[mn]
            o.bases = [c.name] + [x for x in o.bases if x != c.name]
            o.parent = o.parent or c.parent
        if c.parent == "" and others:
            c.parent = next((project.modules[mn].parent for mn in others if project.modules[mn].parent), "")
        for mn in others:
            # only the class declares the constant: a lookup lands on it and
            # reaches the reopenings through its bases (a namespace reopened in
            # a hundred files made every lookup walk a hundred modules)
            r.full_of[mn] = full
            project.modules[mn].decls = []
    for full in lib_reopened:
        for mn in idx[full]:
            r.full_of[mn] = full
            project.modules[mn].decls = []
    return r


def _bare_calls_on_self(project, rb: list) -> None:
    """A bare call in a class is `self.x`: the class, its ancestors, and (a
    mixin calling its includer's method, a base class its subclasses') the
    classes below it. A top-level `def` the file can reach (a method of
    every object) keeps the file's scope."""
    top_defs = _top_defs_seen(project, rb)
    for m in rb:
        if not m.owner:
            continue
        seen = top_defs.get(m.owner, set())
        for f in m.funcs:
            for c in f.calls:
                if not c.chain and not c.qual and c.name not in seen:
                    c.chain, c.qual = ("self",), "self"


def _rewrite_constants(rb: list, r: "_Resolver") -> None:
    """Every constant in a chain, a binding or a return type as the full name
    of the class it means (`Site` in module Jekyll is `Jekyll::Site`); a gem's
    stays as written (infer reads an undeclared constant as a library's)."""
    for m in rb:
        nest = r.nesting(m)
        for k, v in list(m.bound.items()):
            m.bound[k] = r.rewrite_held(nest, v)
        for f in m.funcs:
            if f.returns.startswith("expr:"):
                f.returns = r.rewrite_held(nest, f.returns)
            for k, v in list(f.bound.items()):
                if v.startswith("lib:"):
                    if r.resolve(nest, v[4:]):
                        del f.bound[k]  # a project constant's block: we don't know what it yields
                    else:
                        f.bound[k] = f"expr:{v[4:]}\x1fyield()"  # what a library yields is the library's
                    continue
                f.bound[k] = r.rewrite_held(nest, v)
            for c in f.calls:
                if not c.chain:
                    continue
                c.chain = tuple(r.index_new([r.const(nest, s) if s[:1].isupper() else s for s in c.chain]))
                if c.chain[-1][:1].isupper():
                    c.qual = c.chain[-1].replace("::", ".")
                    if c.name == "new" and len(c.chain) == 1:
                        c.name = r.constructor(c.chain[0])


def _infer_types(project, rb: list, r: "_Resolver") -> None:
    """Parameters and fields typed from their callers, then the calls whose
    receiver is a library's value marked so."""
    # the type work sees each class's subclasses and includers (core fills
    # project.children for real once linking is done)
    from collections import defaultdict
    from .infer import Types
    saved, project.children = project.children, defaultdict(list)
    t = Types(project)
    for m in rb:
        for a in t.ancestors(m.name)[1:]:
            project.children[a].append(m.name)
    try:
        _ctor_params(project, rb, r)
        for m in rb:
            for k, v in list(m.bound.items()):
                if v.startswith("alt:"):
                    del m.bound[k]  # alternatives that never agreed
        t = Types(project)
        _library_receivers(project, rb, t)
        _core_on_unknown(project, rb, t)
    finally:
        project.children = saved


def _library_receivers(project, rb: list, t) -> None:
    """`last_response.body`, `get("/").headers`: a receiver that is a bare
    call reaching no project method (Rack::Test's, Kernel's) is a library
    value, so the call on it is the library's too: qualifier `Object`."""
    from .core import _in_scope
    reach: dict[tuple[str, str], bool] = {}

    def library(m, fm, f, head: str, depth: int = 0) -> bool:
        if head in f.bound and f.bound[head].startswith("expr:") and depth < 3:
            # `error = assert_raises(Foo) do ... end`: a local holding one
            return library(m, fm, f, f.bound[head][5:].split("\x1f")[0], depth + 1)
        if not head.endswith("()") or not (head[:1].islower() or head[:1] == "_"):
            return False
        name = head[:-2]
        key = (m.name, name)
        if key not in reach:
            hits = t.method([m.name], name)[0] if m.owner else []
            reach[key] = bool(hits or _in_scope(project, fm.name, name))
        return not reach[key]

    for m in rb:
        fm = project.modules.get(m.owner, m) if m.owner else m
        for f in m.funcs:
            for c in f.calls:
                if c.chain and c.qual != "Object" and library(m, fm, f, c.chain[0]):
                    c.qual = "Object"


def _core_on_unknown(project, rb: list, t) -> None:
    """`x.each`, `opts.fetch`, `@data.key?` where nothing types x / opts /
    @data: Ruby's method. Its qualifier becomes `Object` (a library module,
    so core counts it as reaching nothing; "named" in stats still shows it
    when a project function has the name). The chain stays as written."""
    for m in rb:
        for f in m.funcs:
            for c in f.calls:
                if c.chain and c.name in _CORE_METHODS and c.qual != "Object" and t.chain(m, f, c.chain) is None:
                    c.qual = "Object"


def _top_defs_seen(project, rb: list) -> dict[str, set[str]]:
    """File -> the top-level `def`s it can call: its own and those of the
    files it requires (and they require). A benchmark script's `def site`
    is not every class's `site`."""
    own = {m.name: {f.name for f in m.funcs if f.kind != "top"} for m in rb if not m.owner}
    out = {}
    for mn in own:
        seen, todo, names = set(), [mn], set()
        while todo:
            cur = todo.pop()
            if cur in seen or cur not in own:
                continue
            seen.add(cur)
            names |= own[cur]
            todo += project.modules[cur].reexports
        out[mn] = names
    return out


def _link_requires(project, rb: list) -> None:
    files = {m.name for m in rb if not m.owner}
    suffix: dict[str, list[str]] = {}
    for mn in files:
        parts = mn.split("/")
        for k in range(len(parts)):
            suffix.setdefault("/".join(parts[k:]), []).append(mn)
    for m in rb:
        if m.owner:
            continue
        got = []
        for u in m.uses:
            if not u.startswith("rb:"):
                got.append(u)
                continue
            d, spec, rel = u[3:].split("\0")
            spec = re.sub(r"\.rb$", "", spec)
            hit = ""
            if rel:
                hit = os.path.normpath(os.path.join(d, spec))
                hit = hit if hit in files else ""
            else:
                hit = _pick_require(suffix.get(spec, []), spec, d)
            if hit and hit != m.name and hit not in got:
                got.append(hit)
        # A require loads a file's top-level methods (and, in turn, what it
        # requires): a bare call follows them. It imports no names, so it
        # doesn't widen what the file "sees" for a method-name match.
        m.uses[:] = []
        m.reexports[:] = got


def _pick_require(hits: list[str], spec: str, d: str) -> str:
    """`require "jekyll/site"`: the file under a load path (`lib/`, `test/`)
    nearest the requiring file."""
    if len(hits) <= 1:
        return hits[0] if hits else ""
    def root(h):
        return h[:len(h) - len(spec)].rstrip("/")
    ranked = sorted(hits, key=lambda h: (not root(h).endswith(("lib", "test", "spec")) and root(h) != "",
                                         -len(os.path.commonprefix([root(h) + "/", d + "/"])), len(h)))
    return ranked[0]


def _canonical(project, full: str, names: list[str]) -> str:
    """The class among a constant's reopenings: the one stating a superclass,
    then the file named after it (`site.rb` for Site), then the most methods."""
    last = re.sub(r"(?<=[a-z0-9])([A-Z])", r"_\1", full.split("::")[-1]).lower()

    def key(mn):
        m = project.modules[mn]
        named = os.path.splitext(os.path.basename(m.file))[0] == last
        return (not any(b.startswith("S") for b in m.bases), not named, -len(m.funcs), m.file)
    return min(names, key=key)


class _Resolver:
    """Ruby constant lookup: the lexical scopes (innermost first), then the
    innermost class's ancestors, then the top level."""

    def __init__(self, project, idx, canon):
        self.p, self.idx, self.canon = project, idx, canon
        self.globals: list[str] = []  # namespaces a top-level `include` mixes into Object
        self.ext_super: dict[str, str] = {}  # class -> a library superclass it names (`< Hash`)
        self.full_of: dict[str, str] = {}  # a reopening's module -> the constant it reopens
        self.full_bases: dict[str, list[str]] = {}
        self.aliases: dict[str, tuple[list[str], str]] = {}  # `A::Stream` -> (A's nesting, "Sinatra::Helpers::Stream")

    def full(self, mn: str) -> str:
        """The constant a class module defines (or reopens), "" for a file."""
        m = self.p.modules[mn]
        return m.decls[0] if m.owner and m.decls else self.full_of.get(mn, "")

    def nesting(self, m) -> list[str]:
        full = self.full(m.name)
        if not full:
            return []
        parts = full.split("::")
        return ["::".join(parts[:k]) for k in range(len(parts), 0, -1)]

    def _ancestors_full(self, full: str) -> list[str]:
        out, todo = [], [full]
        while todo and len(out) < 20:
            cur = todo.pop(0)
            if cur in out:
                continue
            out.append(cur)
            todo += self.full_bases.get(cur, [])
        return out[1:]

    def resolve(self, nest: list[str], written: str, depth: int = 0) -> str:
        if written.startswith("::"):
            return written[2:] if written[2:] in self.idx else ""
        for pre in nest + [""]:  # `Minitest::Test`: the project reopens it without declaring Minitest
            cand = f"{pre}::{written}" if pre else written
            if cand in self.aliases and depth < 4:  # `Stream = Sinatra::Helpers::Stream` in that scope
                a_nest, target = self.aliases[cand]
                return self.resolve(a_nest, target, depth + 1)
            if cand in self.idx:
                return cand
        head, _, rest = written.partition("::")
        base = ""
        for pre in nest + [""]:
            cand = f"{pre}::{head}" if pre else head
            if cand in self.idx:
                base = cand
                break
        if not base:
            for anc in (self._ancestors_full(nest[0]) if nest else []) + self.globals:
                if f"{anc}::{head}" in self.idx:
                    base = f"{anc}::{head}"
                    break
        if not base:
            return ""
        full = f"{base}::{rest}" if rest else base
        return full if full in self.idx else ""

    def index_new(self, segs: list[str]) -> list[str]:
        """`Configuration[...]` on a project subclass of Hash / Array / Set
        that defines no `self.[]`: Ruby's `[]` builds one, like `new`."""
        out = list(segs)
        for k in range(1, len(out)):
            if out[k] in ("[]", "[]()") and out[k - 1] in self.canon and self.collection_class(out[k - 1]):
                out[k] = "new()"
        return out

    def collection_class(self, full: str) -> bool:
        seen, todo = set(), [self.canon[full]]
        while todo:
            mn = todo.pop(0)
            if mn in seen or mn not in self.p.modules:
                continue
            seen.add(mn)
            if f"{mn}.[]" in self.p.funcs:
                return False
            m = self.p.modules[mn]
            if m.decls and self.ext_super.get(m.decls[0]) in ("Hash", "Array", "Set"):
                return True
            todo += m.bases + ([m.parent] if m.parent else [])
        return False

    def const(self, nest: list[str], written: str) -> str:
        """A written constant as the full name of the class it means; as
        written when the project declares no such class (a gem's, Ruby's)."""
        return self.resolve(nest, written) or written.lstrip(":")

    def bases(self, m) -> None:
        sup, mixins, fulls = "", [], []
        for spec in m.bases:
            if spec[:1] not in ("S", "M") or "\0" not in spec:
                continue
            where, written = spec[1:].split("\0")
            nest = ["::".join(where.split("::")[:k]) for k in range(len(where.split("::")), 0, -1)] if where else []
            full = self.resolve(nest, written)
            if not full and spec[0] == "S" and m.decls:
                self.ext_super[m.decls[0]] = written.split("::")[-1]
            if full:
                fulls.append(full)
                tgt = self.canon[full]
                if tgt == m.name:
                    continue
                if spec[0] == "S":
                    sup = tgt
                else:
                    mixins.append(tgt)
        mixins.reverse()  # the last included module is looked up first
        m.bases = mixins + ([sup] if sup else [])
        m.parent = sup
        m.unresolved = True  # Object / Kernel / BasicObject: every class has a library ancestor
        if m.decls:
            self.full_bases.setdefault(m.decls[0], [])
            self.full_bases[m.decls[0]] += [x for x in fulls if x != m.decls[0]]

    def rewrite_held(self, nest: list[str], v: str) -> str:
        if v.startswith("alt:"):
            return "alt:" + "\x1e".join(self.rewrite_held(nest, a) for a in v[4:].split("\x1e"))
        if not v.startswith("expr:"):
            return v
        segs = v[5:].split("\x1f")
        return "expr:" + "\x1f".join(self.index_new([self.const(nest, s) if s[:1].isupper() else s for s in segs]))

    def constructor(self, full: str) -> str:
        """`Foo.new`: Foo's (or an ancestor's) own `new` if it defines one,
        else the `initialize` it runs."""
        if full not in self.canon:
            return "new"
        p = self.p
        seen, todo = set(), [self.canon[full]]
        found_init = False
        while todo:
            mn = todo.pop(0)
            if mn in seen or mn not in p.modules:
                continue
            seen.add(mn)
            if f"{mn}.new" in p.funcs:
                return "new"
            found_init = found_init or f"{mn}.initialize" in p.funcs
            m = p.modules[mn]
            todo += m.bases + ([m.parent] if m.parent else [])
        return "initialize" if found_init else "new"


# --- constructor parameters, from their callers ---------------------------------

def _ctor_params(project, rb: list, r: "_Resolver") -> None:
    """`def initialize(site); @site = site`: a parameter every project call
    site (`Reader.new(self)`, `super(site)`) passes the same type to has that
    type, and so has the field it is stored in. Ruby states no parameter
    types; without this, every `@site.x` in a class built that way was a
    name guess. One caller we can't type (or two that disagree) says nothing.
    When the classes sharing an `initialize` disagree (each Drop wraps its
    own kind of object), each subclass's fields take its own callers' type.
    Repeated: Site -> Reader(site) -> CollectionReader(site)."""
    from .infer import Types
    p = project
    by_init: dict[str, dict[str, list]] = {}  # initialize -> class built -> call sites
    for m in rb:
        for f in m.funcs:
            for c in f.calls:
                if c.name != "initialize":
                    continue
                if c.qual == "super":
                    cls, g = m.name, (_find_up(p, m.parent, "initialize") if m.parent else None)
                elif len(c.chain) == 1 and c.chain[0] in r.canon:
                    cls = r.canon[c.chain[0]]
                    g = _find_up(p, cls, "initialize")
                else:
                    g = None
                if g is not None and g.file.endswith(EXT):
                    by_init.setdefault(g.id, {}).setdefault(cls, []).append((m, f, c))
    # the fields each initialize builds from its parameters, as written
    raw: dict[str, dict[str, list[str]]] = {}
    for gid in by_init:
        g = p.funcs[gid]
        params = set(_positional(g))
        raw[gid] = {k: v[5:].split("\x1f") for k, v in p.modules[g.module].bound.items()
                    if v.startswith("expr:") and v[5:].split("\x1f")[0] in params}
    texts: dict[str, tuple[str, list[int]]] = {}
    for _ in range(8):
        t = Types(project)
        changed = False
        for gid, per_cls in by_init.items():
            g = p.funcs[gid]
            own_test = _test_file(g.file)
            changed = _option_fields(p, r, t, texts, g, per_cls, raw[gid], own_test) or changed
            for k, name in enumerate(_positional(g)):
                tys = {cls: _agreed(p, r, t, texts, calls, k, own_test) for cls, calls in per_cls.items()}
                every = set(tys.values()) - {None}
                if len(every) == 1 and "" not in every:
                    ty = every.pop()
                    if g.bound.get(name) != ty:
                        g.bound[name] = ty
                        changed = True
                    changed = _type_fields(p.modules[g.module], raw[gid], name, ty) or changed
                    continue
                # each class built through this initialize types its own fields;
                # the defining class only when every other one has a type of its own
                # (a subclass with none would inherit the defining class's)
                own_ok = all(ty for cls, ty in tys.items() if cls != g.module)
                for cls, ty in tys.items():
                    if ty and (cls != g.module or own_ok):
                        changed = _type_fields(p.modules[cls], raw[gid], name, ty) or changed
        changed = _method_params(p, rb, r, t, texts) or changed
        changed = _settle_alternatives(p, rb, Types(project), r) or changed
        if not changed:
            break


def _method_params(p, rb: list, r, t, texts) -> bool:
    """The same for any method: `def read(site)` called only as
    `reader.read(site)` with a Site. Only when every call of that name in
    the project resolves (one we can't place might pass something else), the
    name is never a symbol (`send(:x)`, `method(:x)`, a callback), and the
    method doesn't check what it got (`respond_to?`, `is_a?`, `case x`)."""
    from .core import _in_scope
    sites: dict[str, list] = {}
    unsure: set[str] = set()
    for m in rb:
        fm = p.modules.get(m.owner, m) if m.owner else m
        for f in m.funcs:
            unsure |= f.refs
            for c in f.calls:
                if c.name == "initialize" or c.qual in ("super", "Object"):
                    continue
                if not c.chain:
                    hits = _in_scope(p, fm.name, c.name)
                else:
                    v = t.chain(m, f, c.chain)
                    if v is not None and v[0] == "ext":
                        continue  # a library's method
                    if v is None or v[0] not in ("inst", "static") or not v[2]:
                        unsure.add(c.name)
                        continue
                    hits = t.method(list(v[1]), c.name)[0]
                    if v[0] == "static":
                        hits = list({h.id: h for mn in v[1] for h in _in_scope(p, mn, c.name)}.values()) or hits
                if len(hits) == 1:
                    sites.setdefault(hits[0].id, []).append((m, f, c))
                elif hits:
                    unsure.add(c.name)
    changed = False
    for gid, calls in sites.items():
        g = p.funcs[gid]
        if g.name in unsure or g.name == "initialize" or g.kind in ("test", "top") or not g.file.endswith(EXT):
            continue
        for k, name in enumerate(_positional(g)):
            if name in g.bound or re.search(rf"\b{name}\s*&?\.\s*(?:respond_to\?|is_a\?|kind_of\?|instance_of\?|class\b)"
                                            rf"|\bcase\s+{name}\b|===\s*{name}\b", g.code):
                continue
            ty = _agreed(p, r, t, texts, calls, k, _test_file(g.file))
            if ty:
                g.bound[name] = ty
                changed = True
    return changed


def _block_forwarders(rb: list) -> None:
    """`def update_file(path, &block) ... File.open(path, 'w', &block)`: a
    block given to update_file is File's, so `update_file(p) do |f|` has f
    as the library's (when every method of that name hands it on alike)."""
    fwd: dict[str, set] = {}
    for m in rb:
        for f in m.funcs:
            if not re.match(r"\s*def\s+(?:self\.)?\w+[?!]?\s*\([^)]*&(\w+)", f.code):
                fwd.setdefault(f.name, set()).add(None)
                continue
            blk = re.match(r"\s*def\s+(?:self\.)?\w+[?!]?\s*\([^)]*&(\w+)", f.code).group(1)
            hm = re.search(rf"(?<![\w.:])({'|'.join(sorted(_STDLIB_YIELDS))})\s*\.\s*\w+[?!]?\s*\([^()\n]*&{blk}\s*\)", f.code)
            fwd.setdefault(f.name, set()).add(hm.group(1) if hm else None)
    names = {n: next(iter(cs)) for n, cs in fwd.items() if len(cs) == 1 and None not in cs}
    if not names:
        return
    pat = re.compile(rf"(?<![\w.:@])({'|'.join(map(re.escape, names))})\b[^\n{{]*?(?:\bdo|\{{)\s*\|([^|\n]*)\|")
    for m in rb:
        for f in m.funcs:
            for cm in pat.finditer(f.code):
                for nm in re.findall(r"[a-z_]\w*", cm.group(2)):
                    f.bound.setdefault(nm, "lib:" + names[cm.group(1)])


def _option_fields(p, r, t, texts, g, per_cls: dict, raw: dict, own_test: bool) -> bool:
    """`@site = relations[:site]` with every caller passing `:site => <a Site>`
    (or `site: ...`): the field is a Site."""
    params = _positional(g)
    changed = False
    for iv, segs in raw.items():
        km = re.match(r"\[:(\w+)\]$", segs[1]) if len(segs) == 2 else None
        if not km or segs[0] not in params:
            continue
        k, key = params.index(segs[0]), km.group(1)
        tys = {cls: _agreed(p, r, t, texts, calls, k, own_test, key) for cls, calls in per_cls.items()}
        every = set(tys.values()) - {None}
        targets = [g.module] if len(every) == 1 and "" not in every else [c for c, ty in tys.items() if ty and c != g.module]
        for cls in targets:
            ty = every.pop() if cls == g.module else tys[cls]
            m = p.modules[cls]
            if m.bound.get(iv) != ty:
                m.bound[iv] = ty
                changed = True
    return changed


def _type_fields(m, raw: dict, param: str, ty: str) -> bool:
    """Fields built from `param` (`@site = site`, `@site = doc.site`), typed."""
    changed = False
    for iv, segs in raw.items():
        if segs[0] == param:
            val = "\x1f".join([ty, *segs[1:]])
            if m.bound.get(iv) != val:
                m.bound[iv] = val
                changed = True
    return changed


def _settle_alternatives(p, rb: list, t, r) -> bool:
    """A field set to several expressions ("alt:"): the class they all
    build, or nothing."""
    changed = False
    for m in rb:
        for k, v in list(m.bound.items()):
            if not v.startswith("alt:"):
                continue
            fulls = set()
            for a in v[4:].split("\x1e"):
                got = t.chain(m, None, tuple(a[5:].split("\x1f"))) if a.startswith("expr:") else None
                fulls.add(_full_of(r, got))
            if "" in fulls or len(fulls) != 1:
                continue  # (kept: a constructor's type may settle it on the next round)
            m.bound[k] = f"expr:{fulls.pop()}\x1fnew()"
            changed = True
    return changed


def _full_of(r, v) -> str:
    """The one class (full name) a value is an instance of, or ""."""
    if v is None:
        return ""
    if v[0] == "ext":
        return "Object"  # some library's value (`Rack::Builder.new` loses its name): calls on it are the library's
    if v[0] == "inst" and v[2]:
        fulls = {r.full(mn) for mn in v[1]} - {""}
        return next(iter(fulls)) if len(fulls) == 1 else ""
    return ""


def _find_up(p, mn: str, name: str):
    seen, todo = set(), [mn]
    while todo:
        cur = todo.pop(0)
        if cur in seen or cur not in p.modules:
            continue
        seen.add(cur)
        g = p.funcs.get(f"{cur}.{name}")
        if g is not None:
            return g
        m = p.modules[cur]
        todo += m.bases + ([m.parent] if m.parent else [])
    return None


def _positional(g: Func) -> list[str]:
    """The positional parameters of a def, in order (none after a splat)."""
    m = re.match(r"\s*def\s+(?:[\w@]+\.)?\w+[?!=]?\s*\(", g.code)
    if not m:
        return []
    out = []
    for part in _split_args(g.code, m.end() - 1):
        part = g.code[part[0]:part[1]].strip()
        pm = re.match(r"([a-z_]\w*)\s*(?:=|$)", part)
        if not pm:
            break  # *rest, key:, &blk: what follows isn't positional
        out.append(pm.group(1))
    return out


def _split_args(code: str, k: int) -> list[tuple[int, int]]:
    """code[k] is "(": the (start, end) of each top-level argument."""
    out, depth, a = [], 0, k + 1
    n = len(code)
    while k < n:
        ch = code[k]
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
            if depth == 0:
                if code[a:k].strip():
                    out.append((a, k))
                return out
        elif ch == "," and depth == 1:
            out.append((a, k))
            a = k + 1
        k += 1
    return out


def _call_args(text: str, pos: int) -> list[tuple[int, int]] | None:
    """The arguments of the call whose name ends at pos: `(a, b)` or a bare
    `a, b` to the end of the line. None when they can't be read."""
    k = pos
    if k < len(text) and text[k] == "(":
        return _split_args(text, k)
    e = text.find("\n", k)
    e = len(text) if e < 0 else e
    rest = text[k:e]
    if not rest.strip() or rest.lstrip().startswith((".", "{", "do", "if ", "unless ", ")", "]", "}", "&&", "||")):
        return []
    if not rest.startswith((" ", "\t")):
        return None
    cut = re.search(r"\s(?:do|if|unless)\b|\{", rest)
    seg = rest[:cut.start()] if cut else rest
    if seg.rstrip().endswith(","):
        return None  # arguments go on over lines
    return [(k + a - 1, k + b - 1) for a, b in _split_args("(" + seg + ")", 0)]


def _agreed(p, r, t, texts, calls, k: int, own_test: bool = False, key: str = "") -> str | None:
    """The one type every call site passes as argument k, as a binding.
    Product code must all be known; a test's call site (often a double, a
    block parameter) only counts when it is known, and then must agree
    (unless the class is a test's own: then its tests are its callers)."""
    got = set()
    product = 0
    for m, f, c in calls:
        ty = _arg_type(p, r, t, texts, m, f, c, k, key)
        authoritative = own_test or not _test_file(m.file)
        if not ty and authoritative:
            return ""
        if ty:
            got.add(ty)
            product += authoritative
        if len(got) > 1:
            return ""
    if not product:
        return None  # only tests (which agree, or can't be typed): no say either way
    return f"expr:{next(iter(got))}\x1fnew()" if len(got) == 1 else ""


def _file_text(p, texts: dict, m) -> tuple[str, list[int]]:
    fm = p.modules.get(m.owner, m) if m.owner else m
    if fm.file not in texts:
        starts = [0]
        for ln in fm.code[:-1]:
            starts.append(starts[-1] + len(ln) + 1)
        texts[fm.file] = ("\n".join(fm.code), starts)
    return texts[fm.file]


def _arg_type(p, r, t, texts, m, f, c, k: int, key: str = "") -> str:
    """The class (full name) argument k of a call is, or ""."""
    code, starts = _file_text(p, texts, m)
    if c.line - 1 >= len(starts) or c.col < 0:
        return ""
    pos = starts[c.line - 1] + c.col
    word = "super" if c.qual == "super" else "new" if c.name == "initialize" else c.name
    if not code.startswith(word, pos):
        return ""
    args = _call_args(code, pos + len(word))
    if not args or len(args) <= k:
        return ""
    if key:  # the value of `:key => v` / `key: v` among the trailing options
        for a, b in args[k:]:
            km = re.match(rf"\s*(?::{key}\s*=>|{key}:)\s*", code[a:b])
            if km:
                a += km.end()
                break
        else:
            return ""
    a, b = args[k] if not key else (a, b)
    arg = code[a:b].strip()
    if not arg or arg[0] in "*&" or re.match(r"\w+:\s", arg) or "=>" in arg or arg.startswith("{"):
        return ""  # a splat, a block, keywords: not argument k
    params = _params_of(f)
    ch = expression(code[:b], a + len(code[a:b]) - len(code[a:b].lstrip()), lambda n, _p: n in f.bound or n in params)
    ch = tuple(r.const(r.nesting(m), s) if s[:1].isupper() else s for s in ch)
    return _full_of(r, t.chain(m, f, ch) if ch else None)
