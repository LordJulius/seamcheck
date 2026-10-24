//! The shared model every language adapter fills in (model.py).
//!
//! A codebase is a set of Modules; a Module holds Funcs; a Func knows its
//! span, its code (comments and string contents blanked, same line layout)
//! and the calls it makes. Rules, the graph and queries only ever see this
//! model, so adding a language means writing one adapter (`lang::Adapter`).
//!
//! Field for field the Python dataclasses: an adapter ported from Python
//! fills the same fields with the same values, and `parity --parse` compares
//! them file by file (`Module::dump`).

use std::collections::BTreeSet;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// the called function's name
    pub name: String,
    /// what it was called on ("" = unqualified, "super", a module alias, a variable)
    pub qual: String,
    /// 1-based line in the file
    pub line: usize,
    /// what it is called on, step by step: `self.store.get().save()` is
    /// ["self", "store", "get()"]; empty for a bare call (chains.receiver)
    pub chain: Vec<String>,
    /// 0-based column of the name, -1 unknown
    pub col: i64,
    /// C#: how many type arguments it passes, -1 unknown
    pub targs: i64,
}

impl Call {
    pub fn new(name: &str, qual: &str, line: usize) -> Call {
        Call { name: name.into(), qual: qual.into(), line, chain: Vec::new(), col: -1, targs: -1 }
    }
}

#[derive(Debug, Clone)]
pub struct Func {
    pub module: String,
    pub name: String,
    pub file: String,
    /// 1-based, inclusive
    pub start: usize,
    pub end: usize,
    /// the body with comments / string contents blanked
    pub code: String,
    pub calls: Vec<Call>,
    /// names referenced without a call (callbacks, "method" strings)
    pub refs: BTreeSet<String>,
    /// local variable -> what it holds ("group:<id>", "type:<Name>", ...)
    pub bound: IndexMap<String, String>,
    /// Kotlin extension function: the receiver type
    pub recv: String,
    /// the declared return type's class, "" unknown
    pub returns: String,
    /// "func", "top" (module-level code), "private", "test"...
    pub kind: String,
    /// C#: its overloads' type-parameter counts ("0,1")
    pub targs: String,
    /// its overloads' declared parameter types
    pub ptypes: String,
    /// what iterating its declared return type gives
    pub relem: String,
    /// C / C++: the namespace a free function is in
    pub ns: String,
    /// "Module.name", kept in sync when the module is renamed
    pub id: String,
}

impl Func {
    pub fn new(module: &str, name: &str, file: &str, start: usize, end: usize, code: String, kind: &str) -> Func {
        Func {
            module: module.into(),
            name: name.into(),
            file: file.into(),
            start,
            end,
            code,
            calls: Vec::new(),
            refs: BTreeSet::new(),
            bound: IndexMap::new(),
            recv: String::new(),
            returns: String::new(),
            kind: kind.into(),
            targs: String::new(),
            ptypes: String::new(),
            relem: String::new(),
            ns: String::new(),
            id: format!("{module}.{name}"),
        }
    }

    pub fn span(&self) -> Span {
        Span {
            module: self.module.clone(),
            name: self.name.clone(),
            file: self.file.clone(),
            start: self.start,
            end: self.end,
            kind: self.kind.clone(),
        }
    }
}

/// Where a function is, without its body: what rules hand around.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub module: String,
    pub name: String,
    pub file: String,
    pub start: usize,
    pub end: usize,
    pub kind: String,
}

#[derive(Debug, Clone, Default)]
pub struct Module {
    pub name: String,
    pub file: String,
    pub lines: usize,
    /// the original lines (suppression comments, snippets)
    pub raw: Arc<Vec<String>>,
    /// lines with comments / string contents blanked
    pub code: Arc<Vec<String>>,
    pub funcs: Vec<Func>,
    /// extends / parent module ("" = none)
    pub parent: String,
    /// local name -> module name
    pub aliases: IndexMap<String, String>,
    /// types this file declares (path-named languages)
    pub decls: Vec<String>,
    /// Kotlin / Java package
    pub package: String,
    /// modules or "pkg:a.b" packages imported whole (Elixir `import`)
    pub uses: Vec<String>,
    /// local name -> (module or "pkg:a.b", name)
    pub imported: IndexMap<String, (String, String)>,
    /// file-level declared types: name -> "type:<Name>"
    pub bound: IndexMap<String, String>,
    /// Elixir `use X` / `use X, :name` (as "X" / "X:name")
    pub using: Vec<String>,
    /// a class that is its own module inside a file (Python): the file's module
    pub owner: String,
    /// declared supertypes as written
    pub bases: Vec<String>,
    /// "class", "ext", "interface"...
    pub kind: String,
    pub unresolved: bool,
    /// an interface's declared methods
    pub methods: Vec<String>,
    /// modules re-exported whole
    pub reexports: Vec<String>,
    /// local names bound to imports from outside the project (core fills it)
    pub external: BTreeSet<String>,
}

impl Module {
    pub fn new(name: &str, file: &str, raw: Arc<Vec<String>>, code: Arc<Vec<String>>) -> Module {
        Module { name: name.into(), file: file.into(), lines: raw.len(), raw, code, ..Default::default() }
    }

    /// The innermost function holding `line` (Module.func_at).
    pub fn func_at(&self, line: usize) -> Option<&Func> {
        let mut best: Option<&Func> = None;
        for f in &self.funcs {
            if f.start <= line && line <= f.end && best.map(|b| f.start >= b.start).unwrap_or(true) {
                best = Some(f);
            }
        }
        best
    }

    /// Renamed (a second module of one name in the project): its functions follow.
    pub fn rename(&mut self, name: String) {
        for f in &mut self.funcs {
            f.module = name.clone();
            f.id = format!("{}.{}", name, f.name);
        }
        self.name = name;
    }

    /// What an adapter produced, as `parity --parse` compares it: every field
    /// but the text (raw / code, compared through the scan) and the function
    /// bodies. The Python side prints the same shape (parity.rs, PY_PARSE).
    pub fn dump(&self) -> Value {
        let funcs: Vec<Value> = self
            .funcs
            .iter()
            .map(|f| {
                json!({
                    "name": f.name, "kind": f.kind, "start": f.start, "end": f.end,
                    "calls": f.calls.iter().map(|c| json!([c.name, c.qual, c.line, c.chain, c.col, c.targs])).collect::<Vec<_>>(),
                    "refs": f.refs.iter().collect::<Vec<_>>(),
                    "bound": f.bound, "recv": f.recv, "returns": f.returns, "targs": f.targs,
                    "ptypes": f.ptypes, "relem": f.relem, "ns": f.ns,
                })
            })
            .collect();
        json!({
            "name": self.name, "lines": self.lines, "parent": self.parent, "aliases": self.aliases,
            "decls": self.decls, "package": self.package, "uses": self.uses,
            "imported": self.imported.iter().map(|(k, (w, n))| (k.clone(), json!([w, n]))).collect::<serde_json::Map<_, _>>(),
            "bound": self.bound, "using": self.using, "owner": self.owner, "bases": self.bases, "kind": self.kind,
            "methods": self.methods, "reexports": self.reexports, "funcs": funcs,
        })
    }
}
