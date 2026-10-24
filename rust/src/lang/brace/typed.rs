//! Declared types, no inference (lang_other._typed / _elements): `name:
//! Type` in Kotlin / TS, `Type* name` in C++, `Foo foo` in Java / C#, Go's
//! `c *Command`, Rust's `x: &mut Foo`, Swift, PHP; and the element type of a
//! container declaration (`List<Foo> xs` -> "xs[]": Foo).

use std::collections::HashSet;

use super::util::{allcaps, re, upper1};
use crate::lang::chains::{generic_name, type_name};

pub const GO_KEYWORDS: &[&str] = &[
    "return", "go", "defer", "case", "range", "package", "import", "type", "func", "else", "chan", "map", "struct",
    "interface", "break", "continue", "var", "const",
];

pub const CPP_NOT_TYPE: &[&str] = &[
    "return", "else", "case", "delete", "new", "throw", "goto", "typedef", "using", "namespace", "sizeof", "co_return",
    "co_await", "co_yield", "operator", "public", "private", "protected", "template", "typename", "static", "inline",
    "virtual", "explicit", "friend", "mutable", "register", "extern", "auto", "void", "int", "char", "bool", "float",
    "double", "long", "short", "unsigned", "signed", "if", "while", "for", "switch", "do", "constexpr", "consteval",
    "decltype", "noexcept", "override", "final", "and", "or", "not", "this",
];

const OTHER_LANGS_PRIMS: &[&str] =
    &["bigint", "error", "str", "number", "symbol", "rune", "boolean", "any", "unknown", "mixed", "object"];

const NOT_CONTAINER: &[&str] =
    &["template", "static_cast", "dynamic_cast", "reinterpret_cast", "const_cast", "return", "new", "typename"];

pub fn go_kw(s: &str) -> bool {
    GO_KEYWORDS.contains(&s)
}

pub fn cpp_not_type(s: &str) -> bool {
    CPP_NOT_TYPE.contains(&s)
}

/// `m.group(1).split("::")[-1].split("<")[0]`
fn cpp_base(t: &str) -> &str {
    let last = t.rsplit("::").next().unwrap_or(t);
    last.split('<').next().unwrap_or(last)
}

fn ts_prim(s: &str) -> &'static str {
    match s {
        "string" => "String",
        "number" => "Number",
        "boolean" => "Boolean",
        "bigint" => "BigInt",
        "symbol" => "Symbol",
        _ => "",
    }
}

pub fn kt_field() -> &'static crate::pyre::Rx {
    re!(r"\b(?:val|var)\s+([a-z_]\w*)\s*:\s*([A-Z]\w*(?:\.[A-Z]\w*)*)(?![\w.])")
}

pub fn kt_new() -> &'static crate::pyre::Rx {
    re!(r"\b(?:val|var)\s+([a-z_]\w*)\s*=\s*([A-Z]\w*)\s*\(")
}

pub fn ts_new() -> &'static crate::pyre::Rx {
    re!(r"(?m)(?:\b(?:const|let|var)\s+|^\s*(?:(?:private|public|protected|readonly|static)\s+)*)([A-Za-z_$][\w$]*)\s*=\s*new\s+([A-Z][\w$]*)\s*[(<]")
}

pub fn go_new() -> &'static crate::pyre::Rx {
    re!(r"\b([A-Za-z_]\w*)\s*:?=\s*(?:&?((?:[A-Za-z_]\w*\.)?[A-Za-z_]\w*)\s*\{|new\(\s*((?:[A-Za-z_]\w*\.)?[A-Za-z_]\w*)\s*\))")
}

/// (name, type) declarations in a stretch of blanked code (`_typed`).
pub fn typed(tk: &str, text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    match tk {
        "kotlin" => {
            let kt_typed =
                re!(r"(?m)(?:\b(?:val|var)\s+|[(,{]\s*|^\s*)([a-z_]\w*)\s*:\s*([A-Z]\w*(?:\.[A-Z]\w*)*)(?![\w.])");
            for pat in [kt_typed, kt_new()] {
                for m in pat.find_iter(text) {
                    out.push((m.s(1).into(), m.s(2).into()));
                }
            }
        }
        "js" => {
            let ts_typed = re!(
                r"(?m)(?:\b(?:const|let|var)\s+|[(,]\s*|^\s*)([A-Za-z_$][\w$]*)\s*\??\s*:\s*([A-Z][\w$]*(?:<[^<>\n]*(?:<[^<>\n]*>[^<>\n]*)*>)?)(?![\w$.<\[])"
            );
            for pat in [ts_typed, ts_new()] {
                for m in pat.find_iter(text) {
                    let g = m.s(2);
                    let t = if g.contains('<') { type_name(g) } else { g.to_string() };
                    if !t.is_empty() {
                        out.push((m.s(1).into(), t));
                    }
                }
            }
            let have: HashSet<String> = out.iter().map(|x| x.0.clone()).collect();
            // `const server = express() as ExtendedHttpTestServer;`: the cast states its type
            for m in re!(r"(?m)\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=[^;\n]*?\sas\s+([A-Z][\w$.]*)\s*;?[ \t]*$")
                .find_iter(text)
            {
                if !have.contains(m.s(1)) {
                    out.push((m.s(1).into(), m.s(2).into()));
                }
            }
            let have: HashSet<String> = out.iter().map(|x| x.0.clone()).collect();
            // `(s: string)`, `cells: string[]`, `n?: number`: the runtime's String / Array / Number
            for m in re!(r"(?m)(?:\b(?:const|let|var)\s+|[(,]\s*)([A-Za-z_$][\w$]*)\s*\??\s*:\s*(string|number|boolean|bigint|symbol|[A-Z][\w$.]*(?=\[\]))(\[\])?(?=\s*[,)=;]|\s*$)")
                .find_iter(text)
            {
                if !have.contains(m.s(1)) {
                    let t = if m.get(3).map(|s| !s.is_empty()).unwrap_or(false) { "Array" } else { ts_prim(m.s(2)) };
                    out.push((m.s(1).into(), t.into()));
                }
            }
        }
        "cpp" | "jvm" => {
            let cpp_typed = re!(r"(?m)(?<![\w:>])([A-Z]\w*)\s*[\*&]+\s*([a-z_]\w*)\s*(?=[,;=)\[{]|:(?!:)|$)");
            let cpp_value = re!(
                r"(?m)(?:^|[(,])\s*(?:(?:const|final|this|using|await\s+using|out|ref|in|params|scoped|readonly)\s+)*([A-Z]\w*(?:\.[A-Z]\w*)*(?:<[^;=(){}]*>)?)\??\s+([a-z_]\w*)\s*(?=[({;=,)]|:(?!:)|in\s)"
            );
            let pats: Vec<&crate::pyre::Rx> = if tk == "cpp" { vec![cpp_typed, cpp_value] } else { vec![cpp_value] };
            for pat in pats {
                for m in pat.find_iter(text) {
                    if allcaps(m.s(1)) {
                        continue;
                    }
                    let t = if tk == "jvm" { generic_name(m.s(1)) } else { type_name(m.s(1)) };
                    if !t.is_empty() {
                        out.push((m.s(2).into(), t));
                    }
                }
            }
            if tk == "cpp" {
                let have: HashSet<String> = out.iter().map(|x| x.0.clone()).collect();
                // `spdlog::logger oss_logger("oss", sink);`: a local built by its constructor
                for m in re!(r"(?m)^[ \t]*(?:const[ \t]+)?(?:::)?((?:\w+::)*[A-Za-z_]\w*(?:<[^;=(){}]*>)?)[ \t]+([a-z_]\w*)[ \t]*\((?:[^;(){}]|\([^;(){}]*\))*\)[ \t]*;")
                    .find_iter(text)
                {
                    if have.contains(m.s(2)) || cpp_not_type(cpp_base(m.s(1))) || allcaps(m.s(1)) {
                        continue;
                    }
                    let t = type_name(m.s(1));
                    if !t.is_empty() && !cpp_not_type(&t) {
                        out.push((m.s(2).into(), t));
                    }
                }
                let have: HashSet<String> = out.iter().map(|x| x.0.clone()).collect();
                for m in re!(r"(?m)(?<![\w:>.])(?:(?:const|struct|class|union|enum|volatile)\s+)*(?:::)?((?:\w+::)*[A-Za-z_]\w*(?:<[^;=(){}<>]*(?:<[^;=(){}<>]*>)?[^;=(){}<>]*>)?)(?:\s*[\*&]+\s*|\s+)(?:const\s+)?([a-z_]\w*)\s*(?=[,;=)\[{]|$)")
                    .find_iter(text)
                {
                    let g1 = m.s(1);
                    if have.contains(m.s(2)) || cpp_not_type(cpp_base(g1)) || cpp_not_type(m.s(2)) || allcaps(g1) {
                        continue;
                    }
                    let mut t = type_name(g1);
                    if t.is_empty() && OTHER_LANGS_PRIMS.contains(&g1) {
                        t = g1.to_string();
                    }
                    if !t.is_empty() && !cpp_not_type(&t) {
                        out.push((m.s(2).into(), t));
                    }
                }
            }
            if tk == "jvm" {
                for m in re!(r"\b(?:var|val|final)\s+([a-z_]\w*)\s*=\s*new\s+([A-Z]\w*)\s*[(<{]").find_iter(text) {
                    out.push((m.s(1).into(), m.s(2).into()));
                }
                let have: HashSet<String> = out.iter().map(|x| x.0.clone()).collect();
                // `int sourceMember`, `string? name`: the language's own value types
                for m in re!(r"(?m)(?:^|[(,])\s*(?:(?:const|final|this|out|ref|in|params|scoped|readonly)\s+)*(string|object|int|uint|long|ulong|short|ushort|byte|sbyte|bool|char|decimal|double|float)\??\s+([a-z_]\w*)\s*(?=[;=,)])")
                    .find_iter(text)
                {
                    if !have.contains(m.s(2)) {
                        out.push((m.s(2).into(), m.s(1).into()));
                    }
                }
            }
        }
        "rust" => {
            let rs_typed = re!(
                r"(?:\blet\s+(?:mut\s+)?|[(,]\s*(?:mut\s+)?)([a-z_]\w*)\s*:\s*&?(?:'\w+\s+)?(?:mut\s+)?((?:[a-z_]\w*::)*[A-Z]\w*|\[[^\]\n]*\]|(?:u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize|f32|f64|bool|char|str))(?![\w:])"
            );
            let rs_new = re!(
                r"\blet\s+(?:mut\s+)?([a-z_]\w*)\s*(?::[^=]+)?=\s*([A-Z]\w*)\s*(?:::\s*(?:new|default|with_\w+|from\w*|builder)\s*\(|\{)"
            );
            for pat in [rs_typed, rs_new] {
                for m in pat.find_iter(text) {
                    if m.s(2) != "Self" {
                        out.push((m.s(1).into(), m.s(2).into()));
                    }
                }
            }
        }
        "go" => {
            let go_typed =
                re!(r"(?m)(?:\bvar\s+|[(,]\s*)([A-Za-z_]\w*)\s+\*?((?:[A-Za-z_]\w*\.)?[A-Za-z_]\w*)(?=\s*[,)=]|\s*$)");
            for pat in [go_typed, go_new()] {
                for m in pat.find_iter(text) {
                    if go_kw(m.s(1)) || go_kw(m.s(2)) {
                        continue;
                    }
                    let t = if !m.s(2).is_empty() { m.s(2) } else { m.s(3) };
                    out.push((m.s(1).into(), t.into()));
                }
            }
        }
        "swift" => {
            let sw_typed = re!(
                r"(?:\b(?:let|var)\s+|[(,]\s*(?:[A-Za-z_]\w*\s+)?)([a-z_]\w*)\s*:\s*(?:inout\s+|@escaping\s+|@\w+\s+)*\(?\s*(?:any\s+|some\s+)?([A-Z][\w.]*(?:<[^<>=\n]*>)?[?!]?)(?![\w.(])"
            );
            let sw_new = re!(r"\b(?:let|var)\s+([a-z_]\w*)\s*=\s*(?:try[?!]?\s+|await\s+)*([A-Z]\w*)\s*\(");
            for pat in [sw_typed, sw_new] {
                for m in pat.find_iter(text) {
                    let t = type_name(m.s(2));
                    if !t.is_empty() {
                        out.push((m.s(1).into(), t));
                    }
                }
            }
        }
        "php" => {
            let php_typed = re!(
                r"(?:[(,]\s*|\b(?:public|private|protected|readonly)\s+)(?:(?:public|private|protected|readonly)\s+)*\??([A-Z\\][\\\w]*)\s+&?(?:\.\.\.)?(\$\w+)"
            );
            for m in php_typed.find_iter(text) {
                let t = type_name(m.s(1));
                if !t.is_empty() {
                    out.push((m.s(2).into(), t));
                }
            }
            for m in re!(r"(\$\w+)\s*=\s*new\s+([\\\w]+)").find_iter(text) {
                let t = type_name(m.s(2));
                if !t.is_empty() {
                    out.push((m.s(1).into(), t));
                }
            }
        }
        _ => {}
    }
    out
}

/// ("name[]", element type) for a container declaration (`_elements`):
/// the element is the last type argument (a map's value).
pub fn elements(tk: &str, text: &str) -> Vec<(String, String)> {
    if tk == "go" {
        // `cmds []*Command`, `flags map[string]*Flag`: their elements (a map's values)
        return re!(r"\b([A-Za-z_]\w*)\s+(?:\[\d*\]|map\[[^\]\n]+\])\s*\*?((?:[A-Za-z_]\w*\.)?[A-Z]\w*)\b")
            .find_iter(text)
            .iter()
            .filter(|m| !go_kw(m.s(1)))
            .map(|m| (format!("{}[]", m.s(1)), m.s(2).to_string()))
            .collect();
    }
    if !matches!(tk, "cpp" | "jvm" | "kotlin" | "js" | "swift") || (!text.contains('<') && !text.contains('[')) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut hits: Vec<(String, String, String)> = Vec::new();
    if tk == "cpp" || tk == "jvm" {
        for m in re!(r"(?m)(?<![\w>.])([A-Za-z_][\w:.]*)\s*<((?:[^<>;=(){}]|<(?:[^<>]|<[^<>]*>)*>)*)>\s*[\*&]*\s*([A-Za-z_]\w*)\s*(?=[;={(,)\[:]|$)")
            .find_iter(text)
        {
            if !NOT_CONTAINER.contains(&m.s(1)) {
                hits.push((m.s(3).into(), m.s(1).into(), m.s(2).into()));
            }
        }
    } else {
        for m in re!(r"([A-Za-z_$][\w$]*)\s*[?!]?\s*:\s*(?:readonly\s+)?([\w.$]+)\s*<((?:[^<>;=]|<(?:[^<>]|<[^<>]*>)*>)*)>")
            .find_iter(text)
        {
            hits.push((m.s(1).into(), m.s(2).into(), m.s(3).into()));
        }
        for m in re!(r"([A-Za-z_$][\w$]*)\s*[?!]?\s*:\s*(?:readonly\s+)?([A-Z][\w$.]*)\s*\[\s*\]").find_iter(text) {
            let t = type_name(m.s(2));
            if !t.is_empty() {
                out.push((format!("{}[]", m.s(1)), t));
            }
        }
    }
    for (name, cont, args) in hits {
        let (mut depth, mut last) = (0i64, 0usize);
        for (k, ch) in args.char_indices() {
            depth += "<([{".contains(ch) as i64;
            depth -= ">)]}".contains(ch) as i64;
            if ch == ',' && depth == 0 {
                last = k + 1;
            }
        }
        let elem = type_name(&args[last..]);
        if !elem.is_empty() && elem != type_name(&format!("{cont}<{args}>")) && upper1(&elem) {
            out.push((format!("{name}[]"), elem));
        }
    }
    out
}
