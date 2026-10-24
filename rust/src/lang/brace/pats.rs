//! Function declarations per extension (lang_other.LANGS): the scan family
//! and the patterns whose group names a function.

use std::sync::OnceLock;

use crate::pyre::Rx;

pub const ID: &str = r"[A-Za-z_$][\w$]*";
/// one TS type, as a function type's result reads: `void`, `Promise<T>`, `Foo[]`
const TS_TYPE: &str = r"(?:void|string|number|boolean|unknown|any|never|null|undefined|bigint|object|[A-Z][\w$.]*(?:<[^()\n]*>)?(?:\[\])?)";
const STMT: &str = r"(?!(?:return|new|else|throw|await|yield|case|goto|using|lock|fixed|checked|unchecked)\b)";
const SWIFT_MODS: &str = r"(?:(?:@\w+(?:\([^)\n]*\))?|public|private|internal|fileprivate|open|static|class|final|override|convenience|required|mutating|nonmutating|nonisolated|dynamic|optional|package)\s+)*";

pub struct Pat {
    pub rx: Rx,
    /// `(?P<tsstart>)`: a method's start only; the name is group 2
    pub tsstart: bool,
    /// `(?P<dflt>)`: the module's default export, named "default"
    pub dflt: bool,
}

fn p(s: String) -> Pat {
    Pat { tsstart: s.contains("(?P<tsstart>)"), dflt: s.contains("(?P<dflt>)"), rx: Rx::lit(&s) }
}

fn js() -> Vec<Pat> {
    let id = ID;
    let ts = TS_TYPE;
    vec![
        p(format!(r"\bfunction\s*\*?\s*({id})\s*(?:<(?:[^<>()]|<(?:[^<>()]|<[^<>()]*>)*>)*>)?\s*\(")),
        p(format!(r"(?m)^[ \t]*(?:{id}\.)+({id})[ \t]*=[ \t]*(?:async[ \t]+)?function[ \t]*\*?[ \t]*\(")),
        p(format!(
            r"(?m)^[ \t]*(?:{id}\.)+({id})[ \t]*=\s*(?:(?:{id}\.)+{id}[ \t]*=\s*)+(?:async[ \t]+)?function\b[^(\n]*\("
        )),
        p(format!(
            r"(?m)(?P<dflt>)^[ \t]*(?:module\.(exports)|export[ \t]+(default))[ \t]*=?[ \t]*(?:async[ \t]+)?(?:function[ \t]*\*?[ \t]*\(|\((?:[^()]|\([^()]*\))*\)[ \t]*(?::[^=\n]+?)?=>|{id}[ \t]*=>)"
        )),
        p(format!(
            r"(?m)^[ \t]*(?:module\.)?exports\.({id})[ \t]*=[ \t]*(?:async[ \t]+)?(?:\((?:[^()]|\([^()]*\))*\)[ \t]*(?::[^=\n]+?)?=>|{id}[ \t]*=>)"
        )),
        p(format!(
            r"(?m)^[ \t]*(?!this\.)(?:{id}\.)+({id})[ \t]*=[ \t]*(?:async[ \t]+)?(?:\((?:[^()]|\([^()]*\))*\)[ \t]*(?::[^=\n]+?)?=>|{id}[ \t]*=>)"
        )),
        p(format!(
            r"(?m)^[ \t]*this\.({id})[ \t]*=[ \t]*(?:async[ \t]+)?(?:\((?:[^()]|\([^()]*\))*\)[ \t]*(?::[^=\n]+?)?=>|{id}[ \t]*=>)"
        )),
        p(format!(
            r"\b(?:const|let|var)\s+({id})\s*(?::(?:[^=;]|=>){{1,300}}?)?=\s*(?:\((?=\())?(?:async\s+)?(?:function\b|\((?:[^()]|\((?:[^()]|\([^()]*\))*\))*\)\s*(?::\s*[^=]+)?=>|{id}\s*=>)"
        )),
        p(format!(
            r"(?m)^[ \t]*({id})[ \t]*:[ \t]*(?:async[ \t]+)?(?:function\b|\([^()\n]*\)[ \t]*(?::[^=\n]+)?=>|{id}[ \t]*=>)(?![ \t]*{ts}(?:[ \t]*[|&][ \t]*{ts})*[ \t]*(?:[,;)]|$))"
        )),
        p(format!(
            r#"(?m)^[ \t]*(?:(?:public|private|protected|static|async|get|set|override|readonly|export|default)\s+)*({id})[ \t]*(?:<[^<>\n]*(?:<[^<>\n]*(?:<[^<>\n]*>[^<>\n]*)*>[^<>\n]*)*>)?\s*\((?:[^;()'"`]|\([^;()'"`]*\))*\)\s*(?::\s*[^{{;\n]+)?\{{"#
        )),
        p(format!(
            r"(?m)^[ \t]*(?:(?:public|private|protected|static|readonly|override|declare|export|default|const|let|var)[ \t]+)*({id})[ \t]*[?!]?[ \t]*(?::[^=\n<]*(?:<[^;]{{0,800}}?>[^=\n]*)?)?=[ \t]*(?:async[ \t]+)?(?:<[^()]*?>[ \t]*)?(?:\((?:[^()]|\((?:[^()]|\([^()]*\))*\))*\)[ \t]*(?::[^=;]+?)?=>|{id}[ \t]*=>)"
        )),
        p(format!(
            r"(?m)(?P<tsstart>)^[ \t]*(?:(?:public|private|protected|static|async|get|set|override|abstract)[ \t]+)*({id})[ \t]*(?:<[^<>]{{0,400}}?(?:<[^<>]{{0,200}}(?:<[^<>\n]*>[^<>\n]*)*>[^<>]{{0,400}}?)*>)?[ \t]*\("
        )),
    ]
}

fn rust() -> Vec<Pat> {
    vec![p(r"\bfn\s+([A-Za-z_]\w*)".into())]
}

fn kotlin() -> Vec<Pat> {
    vec![p(r"\bfun\s+(?:<[^>]*>\s*)?(?:[\w.]+(?:<[^<>]*(?:<[^<>]*>[^<>]*)*>)?\??\.)?([A-Za-z_]\w*)\s*\(".into())]
}

fn java() -> Vec<Pat> {
    vec![p(format!(
        r"(?m)^[ \t]*(?:@\w+(?:\([^)\n]*\))?[ \t]+)*{STMT}(?:(?:@[\w.]+(?:\([^)\n]*\))?|[\w.]+<[^<>\n]*(?:<[^<>\n]*(?:<[^<>\n]*>[^<>\n]*)*>[^<>\n]*)*>[?\[\]]*|[\w<>\[\],.?]+)[ \t]+){{1,7}}([A-Za-z_]\w*)[ \t]*\((?:[^;{{}}()]|\([^()]*\))*\)\s*(?:throws\s+[\w.,\s]+?)?\s*\{{"
    ))]
}

fn cs() -> Vec<Pat> {
    vec![p(format!(
        r"(?m)^[ \t]*(?:\[[^\]\n]*\][ \t]*)*{STMT}(?:(?:[\w.]+<[^<>\n]*(?:<[^<>\n]*(?:<[^<>\n]*>[^<>\n]*)*>[^<>\n]*)*>[?\[\]]*|[\w<>\[\],.?]+|[\w.]*<?\((?:[^()\n;{{}}=]|\([^()\n]*\))*\)[>?\[\]]*)[ \t]+){{1,6}}([A-Za-z_]\w*)[ \t]*(?:<[^>\n]*>)?[ \t]*\((?:[^;{{}}()]|\([^()]*\))*\)[ \t]*(?::[ \t]*(?:base|this)[ \t]*\((?:[^;{{}}()]|\([^()]*\))*\))?(?:\s*where[^{{;=]*)?\s*(?:\{{|=>)"
    ))]
}

fn go() -> Vec<Pat> {
    vec![p(r"(?m)^func\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)\s*(?:\[[^\]\n]*\]\s*)?\(".into())]
}

fn cpp() -> Vec<Pat> {
    vec![
        p(r"(?m)^[ \t]*(?!(?:return|else|delete|new|throw|case|co_return|co_await|emit|Q_EMIT|goto|using|typedef|if|for|while|switch)\b)(?:template[ \t]*<[^>\n]*>[ \t]*)?(?:[A-Za-z_][\w:<>,\*&~]*[ \t]+){1,6}[\*&]*(?:\w+(?:<[^<>()\n;]*>)?::)*(~?[A-Za-z_]\w*)[ \t]*\(".into()),
        p(r"(?m)^(?:\w+(?:<[^<>()\n;]*>)?::)+(~?[A-Za-z_]\w*)[ \t]*\(".into()),
        p(r"(?m)^(?:(?:static|inline|extern|const|unsigned|signed|struct|enum|union)[ \t]+)*[A-Za-z_]\w*(?:[ \t]*\*)*[ \t]*\n([A-Za-z_]\w*)[ \t]*\((?:[^;{}()]|\([^()]*\))*\)[ \t]*\n?(?:[ \t]*[A-Za-z_][\w \t\*,\[\]]*;[ \t]*\n)*[ \t]*\{".into()),
    ]
}

fn swift() -> Vec<Pat> {
    vec![
        p(r"\bfunc\s+([A-Za-z_]\w*)\s*(?:<[^>\n]*>)?\s*\(".into()),
        p(format!(r"(?m)^[ \t]*{SWIFT_MODS}(init)\s*[?!]?\s*(?:<[^>\n]*>)?\s*\(")),
    ]
}

fn php() -> Vec<Pat> {
    vec![p(r"\bfunction\s+&?\s*([A-Za-z_]\w*)\s*\(".into())]
}

fn qml() -> Vec<Pat> {
    vec![p(format!(r"\bfunction\s+({ID})\s*\(")), p(r"(?m)^[ \t]*(on[A-Z]\w*)[ \t]*:".into())]
}

/// (scan family, function patterns) for an extension (LANGS[ext]).
pub fn lang(ext: &str) -> Option<(&'static str, &'static [Pat])> {
    macro_rules! cell {
        ($f:ident) => {{
            static C: OnceLock<Vec<Pat>> = OnceLock::new();
            C.get_or_init($f).as_slice()
        }};
    }
    Some(match ext {
        ".js" | ".jsx" | ".mjs" | ".cjs" | ".ts" | ".tsx" | ".mts" => ("js", cell!(js)),
        ".rs" => ("rust", cell!(rust)),
        ".kt" | ".kts" => ("kotlin", cell!(kotlin)),
        ".java" => ("c", cell!(java)),
        ".cs" => ("c", cell!(cs)),
        ".go" => ("c", cell!(go)),
        ".c" | ".cpp" | ".cc" | ".cxx" | ".hpp" | ".hh" | ".h" => ("c", cell!(cpp)),
        ".qml" => ("js", cell!(qml)),
        ".swift" => ("swift", cell!(swift)),
        ".php" => ("php", cell!(php)),
        _ => return None,
    })
}

pub const EXTS: &[&str] = &[
    ".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".rs", ".kt", ".kts", ".java", ".cs", ".go", ".c", ".cpp",
    ".cc", ".cxx", ".hpp", ".hh", ".h", ".qml", ".swift", ".php",
];

pub const CPP_EXTS: &[&str] = &[".cpp", ".cc", ".cxx", ".hpp", ".hh", ".h", ".c"];

/// _NOT_FUNC
pub const NOT_FUNC: &[&str] = &[
    "if", "for", "while", "switch", "catch", "return", "function", "else", "do", "try", "with", "new", "typeof",
    "match", "loop", "when", "sizeof", "constructor_", "super",
];

/// _CALL_SKIP (beyond _NOT_FUNC)
const CALL_SKIP_MORE: &[&str] = &[
    "async", "await", "catch", "require", "import", "print", "println", "Some", "Ok", "Err", "Box", "Vec", "String",
    "assert", "setOf", "listOf", "mapOf",
];

pub fn not_func(s: &str) -> bool {
    NOT_FUNC.contains(&s)
}

pub fn call_skip(s: &str) -> bool {
    NOT_FUNC.contains(&s) || CALL_SKIP_MORE.contains(&s)
}

/// _CALL
pub fn call_rx() -> &'static Rx {
    static R: OnceLock<Rx> = OnceLock::new();
    R.get_or_init(|| {
        Rx::lit(&format!(
            r"(?:({ID})\s*(?:\.|::|\?\.|->(?![ \t]))\s*)?\b({ID})\s*(?:::<[^()]*>|<[\w\s,<>\[\]]*>)?\s*\("
        ))
    })
}
