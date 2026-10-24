//! The adapter's patterns and word lists (lang_ruby.py's module level).

use std::collections::HashSet;
use std::sync::OnceLock;

use super::util::Anch;
use crate::pyre::Rx;

pub const ID: &str = r"[A-Za-z_]\w*";
pub const CONST: &str = r"(?:::)?[A-Z]\w*(?:::[A-Z]\w*)*";

pub struct Pats {
    pub tok: Rx,
    pub def_: Anch,
    pub def_any: Anch,
    pub class: Anch,
    pub sclass: Anch,
    pub module: Anch,
    pub test_head: Anch,
    pub define_method: Rx,
    pub class_new: Anch,
    pub anon_class: Rx,
    pub mixin: Anch,
    pub attr: Anch,
    pub delegators: Anch,
    pub rails_delegate: Anch,
    pub let_: Anch,
    pub private: Anch,
    pub require: Anch,
    pub autoload: Anch,
    pub assign: Rx,
    pub assign_at: Anch,
    pub multi: Rx,
    pub iter_block: Rx,
    pub block_params: Rx,
    pub call_block: Rx,
    pub const_block: Rx,
    pub rescue_var: Rx,
    pub rescue_bind: Rx,
    pub self_set: Rx,
    pub for_var: Rx,
    pub ivar_set: Rx,
    pub ivar_set_at: Anch,
    pub word: Rx,
    pub alias: Anch,
    pub symbol: Rx,
    pub yard_ret: Anch,
    pub yard_param: Anch,
    pub yard_param2: Anch,
    pub sig_ret: Rx,
    pub sig_param: Rx,
    pub expr_head: Anch,
    pub expr_name: Anch,
    pub number: Anch,
    pub sym_lit: Anch,
    pub pct_lit: Anch,
    pub lower_ident: Rx,
    pub push: Rx,
}

pub fn pats() -> &'static Pats {
    static P: OnceLock<Pats> = OnceLock::new();
    P.get_or_init(|| {
        let c = CONST;
        let id = ID;
        Pats {
            tok: Rx::lit(r"(?<![\w.$@:])(class|module|def|if|unless|while|until|case|begin|for|do|end)(?![\w?!]|:(?!:))"),
            def_: Anch::new(&format!(
                r"def\s+(?:(self|{c}|@{{0,2}}[a-z_]\w*)\s*\.\s*)?({id}[?!=]?|\[\]=?|===?|<=>|=~|![=~]?|[-+*/%<>~^&|]+@?|`)"
            )),
            def_any: Anch::new(r"def\s+()(\S+)"),
            class: Anch::new(&format!(r"class\s+({c})(?:\s*<\s*({c}))?")),
            sclass: Anch::new(r"class\s*<<\s*\w+"),
            module: Anch::new(&format!(r"module\s+({c})")),
            test_head: Anch::new(r#"^\s*(?:it|specify|example|scenario|test|should|its)\b\s*\(?\s*(?:["':]|$|do\b)"#),
            define_method: Rx::lit(r#"\bdefine_method\s*\(?\s*(?::|["'])(\w+[?!=]?)"#),
            class_new: Anch::new(&format!(
                r"^\s*([A-Z]\w*)\s*=\s*(?:(Class|Struct|Data)\.(?:new|define))\b\s*(?:\(\s*({c}))?"
            )),
            anon_class: Rx::lit(&format!(r"(?<![\w:])(Class|Module)\.new\b\s*(?:\(\s*({c})\s*\))?\s*$")),
            mixin: Anch::new(&format!(r"^\s*(include|extend|prepend)\s+((?:{c})(?:\s*,\s*{c})*)\s*$")),
            attr: Anch::new(r"^\s*attr_(reader|accessor)\b\s*\(?"),
            delegators: Anch::new(r"^\s*def_delegators?\s+:(@?\w+)\s*,\s*(.*)$"),
            rails_delegate: Anch::new(r"^\s*delegate\s+(.*?)\bto:\s*:(@?\w+)"),
            let_: Anch::new(r"^\s*(?:let!?\s*\(?\s*:(\w+[?!]?)\s*\)?|subject)\s*(\{|do\b)\s*"),
            private: Anch::new(r"^\s*(private|protected|public)\s*$"),
            require: Anch::new(r#"^\s*(require|require_relative|load)\s*\(?\s*["']([^"'#]+)["']"#),
            autoload: Anch::new(r#"^\s*autoload\s*\(?\s*:\w+\s*,\s*["']([^"'#]+)["']"#),
            assign: Rx::lit(r"(?<![\w.@$:])([a-z_]\w*)\s*(?:\|\||&&|\*\*|<<|>>|[-+*/%|&^])?=(?![=~>])"),
            assign_at: Anch::new(r"(?<![\w.@$:])([a-z_]\w*)\s*(?:\|\||&&|\*\*|<<|>>|[-+*/%|&^])?=(?![=~>])"),
            multi: Rx::lit(r"(?m)^\s*((?:\*?@{0,2}[a-z_]\w*\s*,\s*)+\*?@{0,2}[a-z_]\w*)\s*=(?![=~>])"),
            iter_block: Rx::lit(
                r"((?:@{0,2}[a-z_]\w*|[A-Z][\w:]*)(?:\[[^\]\n]*\])?(?:\s*\.\s*[a-z_]\w*[?!]?(?:\([^()\n]*\))?(?:\[[^\]\n]*\])?)*?)\s*\.\s*(?:each|map|select|find|detect|each_with_index|sort_by|reject|any\?|all\?|flat_map|filter|filter_map|count|min_by|max_by|group_by|partition|sum|find_all|none\?|map!|select!|reject!|collect|find_index)\b(?:\([^()\n]*\))?\s*(?:\bdo|\{)\s*\|([^|\n]*)\|",
            ),
            block_params: Rx::lit(r"(?:\bdo|\{)\s*\|([^|\n]*)\|"),
            call_block: Rx::lit(
                r"(?<![\w@$:])([a-z_]\w*[?!]?)(?:(?:\([^()\n]*\))?\s*\{|(?:\([^()\n]*\)|[ \t]+[^\n{|=]*?)?\s*\bdo)\s*\|\s*([a-z_]\w*)",
            ),
            const_block: Rx::lit(&format!(
                r"(?<![\w.:])({c})\s*\.\s*\w+[?!]?(?:\s*\(([^()\n]*)\)|[ \t]+([^\n{{|]*?))?\s*(?:\bdo|\{{)\s*\|([^|\n]*)\|"
            )),
            rescue_var: Rx::lit(r"\brescue\b[^\n=]*=>\s*([a-z_]\w*)"),
            rescue_bind: Rx::lit(&format!(r"\brescue\b\s*((?:{c})(?:\s*,\s*{c})*)?\s*=>\s*([a-z_]\w*)")),
            self_set: Rx::lit(r"(?<![\w@.])self\.(\w+)\s*=(?![=~>])\s*"),
            for_var: Rx::lit(r"\bfor\s+([a-z_]\w*(?:\s*,\s*[a-z_]\w*)*)\s+in\b"),
            ivar_set: Rx::lit(r"(?<![\w@])(@\w+)\s*(?:\|\|)?=(?![=~>])\s*"),
            ivar_set_at: Anch::new(r"(?<![\w@])(@\w+)\s*(?:\|\|)?=(?![=~>])\s*"),
            word: Rx::lit(r"(?<![\w@$])([A-Za-z_]\w*[?!]?)"),
            alias: Anch::new(r"alias\s+:?([\w?!=]+)\s+:?([\w?!=]+)\s*$"),
            symbol: Rx::lit(r"(?<![:\w]):(@{0,2}[A-Za-z_]\w*[?!=]?)"),
            yard_ret: Anch::new(r"^\s*#\s*@return\s+\[([^\]]+)\]"),
            yard_param: Anch::new(r"^\s*#\s*@param\s+\[?(\w+)\]?\s+\[([^\]]+)\]"),
            yard_param2: Anch::new(r"^\s*#\s*@param\s+\[([^\]]+)\]\s+(\w+)"),
            sig_ret: Rx::lit(r"\.returns\(\s*([\w:]+)"),
            sig_param: Rx::lit(r"(\w+):\s*([A-Z][\w:]*)"),
            expr_head: Anch::new(&format!(r"(@@?\w+|\$\w+|{c}|{id}[?!]?)")),
            expr_name: Anch::new(&format!(r"\s*({id}[?!]?)")),
            number: Anch::new(r"[\d_]+(\.\d+)?"),
            sym_lit: Anch::new(r":\w+[?!=]?"),
            pct_lit: Anch::new(r"%([qQwWiIrsx]?)([(\[{<|!/^])"),
            lower_ident: Rx::lit(r"[a-z_]\w*"),
            push: Rx::lit(
                r"(?m)(?<![\w@.:])(@?[a-z_]\w*)\s*(?:<<|\.push\(|\.append\(|\.unshift\()\s*(?:([A-Z][\w:]*)\.new\b|([a-z_]\w*)\s*(?:$|[)\n]|\bif\b|\bunless\b))",
            ),
        }
    })
}

fn set(words: &[&'static str]) -> HashSet<&'static str> {
    words.iter().copied().collect()
}

pub fn opens_after(w: &str) -> bool {
    matches!(w, "then" | "else" | "do" | "begin" | "and" | "or" | "not" | "when" | "in" | "elsif")
}

pub fn is_iter(w: &str) -> bool {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| {
        set(&[
            "each", "map", "select", "find", "detect", "each_with_index", "sort_by", "reject", "any?", "all?",
            "flat_map", "filter", "filter_map", "count", "min_by", "max_by", "group_by", "partition",
            "each_with_object", "sum", "find_all", "none?", "map!", "select!", "reject!", "collect", "each_slice",
            "find_index", "index_by", "to_h", "sort", "uniq",
        ])
    })
    .contains(w)
}

pub fn is_same_elements(w: &str) -> bool {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| {
        set(&[
            "sort", "sort_by", "select", "reject", "filter", "uniq", "reverse", "compact", "to_a", "values", "dup",
            "flatten", "select!", "reject!", "sort!", "uniq!", "compact!", "first", "last", "take", "drop",
        ])
    })
    .contains(w)
}

pub const STDLIB_YIELDS: &[&str] = &[
    "File", "Dir", "IO", "Benchmark", "Open3", "Tempfile", "Zlib", "Timeout", "Mutex", "Thread", "OptionParser", "CSV",
    "Find", "FileUtils", "Pathname", "StringIO", "Socket", "TCPServer", "Net", "Etc", "Process", "Signal", "Logger",
    "Dir::Tmpname", "ERB",
];

pub fn is_keyword(w: &str) -> bool {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| {
        set(&[
            "alias", "and", "begin", "break", "case", "class", "def", "defined?", "do", "else", "elsif", "end",
            "ensure", "false", "for", "if", "in", "module", "next", "nil", "not", "or", "redo", "rescue", "retry",
            "return", "self", "super", "then", "true", "undef", "unless", "until", "when", "while", "yield",
            "__FILE__", "__LINE__", "__dir__", "__method__", "__ENCODING__", "__callee__", "block_given?", "BEGIN",
            "END",
        ])
    })
    .contains(w)
}

pub fn is_declare(w: &str) -> bool {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| {
        set(&[
            "require", "require_relative", "load", "autoload", "include", "extend", "prepend", "attr_reader",
            "attr_writer", "attr_accessor", "attr", "private", "public", "protected", "module_function",
            "private_constant", "private_class_method", "public_class_method", "alias_method", "define_method",
            "def_delegator", "def_delegators", "delegate", "raise", "fail", "lambda", "proc", "loop", "puts", "print",
            "p", "pp", "catch", "throw", "binding", "caller", "sig", "using", "refine",
        ])
    })
    .contains(w)
}

pub fn pct_type(k: &str) -> Option<&'static str> {
    Some(match k {
        "w" | "W" | "i" | "I" => "Array",
        "q" | "Q" | "" | "x" => "String",
        "r" => "Regexp",
        "s" => "Symbol",
        _ => return None,
    })
}

pub fn literal_word(w: &str) -> Option<&'static str> {
    Some(match w {
        "nil" => "NilClass",
        "true" => "TrueClass",
        "false" => "FalseClass",
        _ => return None,
    })
}

pub fn is_same(w: &str) -> bool {
    matches!(w, "tap()" | "dup()" | "clone()" | "freeze()" | "itself()")
}

pub fn is_core_method(w: &str) -> bool {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| {
        "to_s inspect eql? hash is_a? kind_of? instance_of? respond_to? send public_send nil? dup clone freeze frozen?
tap then object_id instance_variable_get instance_variable_set itself
each each_with_index each_with_object each_pair each_key each_value map flat_map collect select filter reject find
detect find_index include? member? any? all? none? count first last min max sort sort_by group_by partition reduce
inject sum to_a to_h to_hash to_ary zip take drop uniq compact flatten length size empty? key? has_key? keys values
fetch dig merge merge! delete push pop shift unshift concat join split strip gsub sub start_with? end_with? downcase
upcase to_sym to_i to_f to_str match match? scan force_encoding encoding values_at slice tr chomp lstrip rstrip
sort! map! select! reject! uniq! compact! each_char bytes chars lines"
            .split_whitespace()
            .collect()
    })
    .contains(w)
}

pub fn modname(rel: &str) -> String {
    rel.rsplit_once('.').map(|(a, _)| a).unwrap_or(rel).to_string()
}

pub fn test_file(rel: &str) -> bool {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    let slashed = format!("/{rel}");
    base.ends_with("_test.rb")
        || base.ends_with("_spec.rb")
        || base.starts_with("test_")
        || slashed.contains("/test/")
        || slashed.contains("/spec/")
}
