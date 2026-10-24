//! Just enough CSS to answer "where does this class's box go" (css.py).
//! `index` reads stylesheets into per-class rules; `props(cls)` folds the
//! base rules in source order (`!important` wins), resolving `var(--x)`.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::pyre::{strip, Rx};

/// An insertion-ordered map (Python dict semantics: overwrite keeps position).
#[derive(Clone, Debug)]
pub struct OMap<V> {
    pub items: Vec<(String, V)>,
}

impl<V> Default for OMap<V> {
    fn default() -> Self {
        OMap { items: Vec::new() }
    }
}

impl<V> OMap<V> {
    pub fn new() -> Self {
        OMap { items: Vec::new() }
    }
    pub fn insert(&mut self, k: &str, v: V) {
        if let Some(slot) = self.items.iter_mut().find(|(x, _)| x == k) {
            slot.1 = v;
        } else {
            self.items.push((k.to_string(), v));
        }
    }
    pub fn get(&self, k: &str) -> Option<&V> {
        self.items.iter().find(|(x, _)| x == k).map(|(_, v)| v)
    }
    pub fn contains(&self, k: &str) -> bool {
        self.get(k).is_some()
    }
    pub fn iter(&self) -> impl Iterator<Item = &(String, V)> {
        self.items.iter()
    }
}

#[derive(Debug)]
pub struct Rule {
    pub file: String,
    pub line: usize,
    pub selector: String,
    pub decls: OMap<(String, bool)>,
    pub order: usize,
}

pub type Props = OMap<(String, Arc<Rule>)>;

#[derive(Default, Debug)]
pub struct Sheet {
    /// class -> its base rules (insertion order kept)
    pub base: OMap<Vec<Arc<Rule>>>,
    pub variant: OMap<Vec<Arc<Rule>>>,
    pub vars: HashMap<String, String>,
    /// class -> its effective props, folded once after indexing
    folded: HashMap<String, Arc<Props>>,
    empty: Arc<Props>,
}

fn rx(cell: &'static OnceLock<Rx>, p: &str) -> &'static Rx {
    cell.get_or_init(|| Rx::lit(p))
}

impl Sheet {
    /// Effective base declarations of a class: prop -> (value, the rule that set it).
    pub fn props(&self, cls: &str) -> Arc<Props> {
        self.folded.get(cls).cloned().unwrap_or_else(|| self.empty.clone())
    }

    fn fold(&self, rules: &[Arc<Rule>]) -> Props {
        let mut out: OMap<(String, Arc<Rule>, bool)> = OMap::new();
        let mut rules: Vec<&Arc<Rule>> = rules.iter().collect();
        rules.sort_by_key(|r| r.order);
        for r in rules {
            for (p, (v, imp)) in r.decls.iter() {
                if let Some(cur) = out.get(p) {
                    if cur.2 && !imp {
                        continue;
                    }
                }
                out.insert(p, (v.clone(), r.clone(), *imp));
            }
        }
        let mut res = OMap::new();
        for (p, (v, r, _)) in out.items {
            res.insert(&p, (self.resolve(&v), r));
        }
        res
    }

    pub fn resolve(&self, value: &str) -> String {
        static V: OnceLock<Rx> = OnceLock::new();
        let re = rx(&V, r"var\(\s*(--[\w-]+)\s*(?:,\s*([^)]*))?\)");
        let mut value = value.to_string();
        for _ in 0..4 {
            let Some(m) = re.search(&value) else { break };
            let rep = match self.vars.get(m.s(1)) {
                Some(v) => v.clone(),
                None => strip(m.s(2)).to_string(),
            };
            value = format!("{}{}{}", &value[..m.start()], rep, &value[m.end()..]);
        }
        strip(&value).to_string()
    }
}

/// (prelude, body_start, body_end) for each top-level `prelude { body }` in text[start..end].
fn blocks(text: &str, start: usize, end: usize) -> Vec<(String, usize, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = start;
    let mut pre = start;
    while i < end {
        let c = b[i];
        if c == b'{' {
            let mut depth = 1;
            let mut j = i + 1;
            while j < end && depth > 0 {
                if b[j] == b'{' {
                    depth += 1;
                } else if b[j] == b'}' {
                    depth -= 1;
                }
                j += 1;
            }
            out.push((strip(&text[pre..i]).to_string(), i + 1, j - 1));
            i = j;
            pre = j;
            continue;
        }
        if c == b';' || c == b'}' {
            pre = i + 1;
        }
        i += 1;
    }
    out
}

fn decls(body: &str) -> OMap<(String, bool)> {
    let mut out = OMap::new();
    for part in body.split(';') {
        if !part.contains(':') || part.contains('{') {
            continue;
        }
        let (p, v) = part.split_once(':').unwrap();
        let p = strip(p).to_lowercase();
        let v = strip(v);
        let imp = v.ends_with("!important");
        let v = strip(v.strip_suffix("!important").unwrap_or(v)).to_string();
        out.insert(&p, (v, imp));
    }
    out
}

type Slots = (HashMap<String, usize>, HashMap<String, usize>);

fn walk(sheet: &mut Sheet, slots: &mut Slots, order: &mut usize, rel: &str, text: &str, a: usize, b: usize) {
    static AT: OnceLock<Rx> = OnceLock::new();
    static NEST: OnceLock<Rx> = OnceLock::new();
    static SPLIT: OnceLock<Rx> = OnceLock::new();
    static STATE: OnceLock<Rx> = OnceLock::new();
    static CLS: OnceLock<Rx> = OnceLock::new();
    static ONE: OnceLock<Rx> = OnceLock::new();
    for (prelude, s, e) in blocks(text, a, b) {
        if prelude.starts_with('@') {
            if rx(&AT, r"@(media|supports|layer|container|document)\b").match_start(&prelude).is_some() {
                walk(sheet, slots, order, rel, text, s, e);
            }
            continue;
        }
        let mut body = text[s..e].to_string();
        if body.contains('{') {
            body = rx(&NEST, r"[^;{}]*\{[^{}]*\}").sub("", &body);
        }
        let d = decls(&body);
        let line = text[..s].matches('\n').count() + 1;
        for sel in prelude.split(',') {
            let sel = strip(sel);
            if sel == ":root" || sel == "html" || sel == ":root:not([data-theme=\"light\"])" {
                for (p, (v, _)) in d.iter() {
                    if p.starts_with("--") && !sheet.vars.contains_key(p) {
                        sheet.vars.insert(p.clone(), v.clone());
                    }
                }
                continue;
            }
            let parts = rx(&SPLIT, r"\s*[ >+~]\s*").split(sel);
            let last = parts.last().copied().unwrap_or("");
            if rx(&STATE, r":(?!not\b|is\b|where\b|root\b)[\w-]+|::[\w-]+").is_match(last) {
                continue;
            }
            let classes = rx(&CLS, r"\.([\w-]+)").findall(last);
            if classes.is_empty() {
                continue;
            }
            *order += 1;
            let rule =
                Arc::new(Rule { file: rel.to_string(), line, selector: sel.to_string(), decls: d.clone(), order: *order });
            if classes.len() == 1 && rx(&ONE, r"\.[\w-]+").fullmatch(last).is_some() {
                push(&mut sheet.base, &mut slots.0, classes[0], rule);
            } else {
                for c in classes {
                    push(&mut sheet.variant, &mut slots.1, c, rule.clone());
                }
            }
        }
    }
}

fn push(m: &mut OMap<Vec<Arc<Rule>>>, at: &mut HashMap<String, usize>, k: &str, r: Arc<Rule>) {
    match at.get(k) {
        Some(&i) => m.items[i].1.push(r),
        None => {
            at.insert(k.to_string(), m.items.len());
            m.items.push((k.to_string(), vec![r]));
        }
    }
}

/// Read stylesheets [(file, text)] into a Sheet.
pub fn index(texts: &[(String, Arc<String>)]) -> Sheet {
    static COMMENT: OnceLock<Rx> = OnceLock::new();
    let mut sheet = Sheet::default();
    let mut order = 0;
    let mut slots: Slots = (HashMap::new(), HashMap::new());
    for (rel, text) in texts {
        let clean = rx(&COMMENT, r"(?s)/\*.*?\*/")
            .sub_with(text, |m| m.group0().chars().map(|c| if c == '\n' { '\n' } else { ' ' }).collect());
        walk(&mut sheet, &mut slots, &mut order, rel, &clean, 0, clean.len());
    }
    let folded: HashMap<String, Arc<Props>> =
        sheet.base.items.iter().map(|(c, rules)| (c.clone(), Arc::new(sheet.fold(rules)))).collect();
    sheet.folded = folded;
    sheet
}

/// Class names in a JSX className value (string and template literal parts).
pub fn classes_in(attr_value: &str) -> Vec<String> {
    static LIT: OnceLock<Rx> = OnceLock::new();
    static INTERP: OnceLock<Rx> = OnceLock::new();
    static NAME: OnceLock<Rx> = OnceLock::new();
    let mut out = Vec::new();
    for m in rx(&LIT, r#""([^"]*)"|'([^']*)'|`([^`]*)`"#).find_iter(attr_value) {
        let s = [m.get(1), m.get(2), m.get(3)].into_iter().flatten().find(|x| !x.is_empty()).unwrap_or("");
        let s = rx(&INTERP, r"\$\{[^}]*\}").sub(" ", s);
        for n in rx(&NAME, r"-?[A-Za-z_][\w-]*").findall(&s) {
            out.push(n.to_string());
        }
    }
    out
}

/// Flat props: prop -> value.
pub type Flat = HashMap<String, String>;

fn g<'a>(p: &'a Flat, k: &str) -> Option<&'a str> {
    p.get(k).map(|s| s.as_str())
}

pub fn clips(p: &Flat) -> String {
    static CLIPS: OnceLock<Rx> = OnceLock::new();
    static CONTAINS: OnceLock<Rx> = OnceLock::new();
    for k in ["overflow", "overflow-x", "overflow-y"] {
        if let Some(v) = g(p, k) {
            if rx(&CLIPS, r"^(hidden|clip|auto|scroll)\b").match_start(v).is_some() {
                return format!("{k}: {v}");
            }
        }
    }
    if let Some(v) = g(p, "contain") {
        if rx(&CONTAINS, r"\b(paint|layout|strict|content)\b").is_match(v) {
            return format!("contain: {v}");
        }
    }
    String::new()
}

pub fn containing_block(p: &Flat) -> String {
    static CONTAINS: OnceLock<Rx> = OnceLock::new();
    let pos = g(p, "position").unwrap_or("static");
    if matches!(pos, "relative" | "absolute" | "fixed" | "sticky") {
        return format!("position: {pos}");
    }
    for k in ["transform", "filter", "perspective"] {
        if let Some(v) = g(p, k) {
            if v != "none" && !v.is_empty() {
                return format!("{k}: {v}");
            }
        }
    }
    if let Some(v) = g(p, "contain") {
        if rx(&CONTAINS, r"\b(paint|layout|strict|content)\b").is_match(v) {
            return format!("contain: {v}");
        }
    }
    String::new()
}

pub fn escapes(p: &Flat) -> String {
    static OUTSIDE: OnceLock<Rx> = OnceLock::new();
    if g(p, "position").unwrap_or("") != "absolute" {
        return String::new();
    }
    for k in ["top", "bottom", "left", "right"] {
        if let Some(v) = g(p, k) {
            if rx(&OUTSIDE, r"^(?:calc\(\s*)?100%").match_start(v).is_some() {
                return format!("{k}: {v}");
            }
        }
    }
    String::new()
}

fn zero(v: &str) -> bool {
    static Z: OnceLock<Rx> = OnceLock::new();
    rx(&Z, r"0(px|%|rem)?").fullmatch(strip(v)).is_some()
}

pub fn covers(p: &Flat) -> bool {
    if let Some(v) = g(p, "inset") {
        if v.split_whitespace().all(zero) {
            return true;
        }
    }
    let e = |k: &str| g(p, k);
    let full_w = e("left").map(zero).unwrap_or(false)
        && (e("right").map(zero).unwrap_or(false) || matches!(g(p, "width").unwrap_or(""), "100%" | "100vw"));
    let full_h = e("top").map(zero).unwrap_or(false)
        && (e("bottom").map(zero).unwrap_or(false) || matches!(g(p, "height").unwrap_or(""), "100%" | "100vh" | "100dvh"));
    full_w && full_h
}

pub fn z_index(p: &Flat) -> Option<i64> {
    static N: OnceLock<Rx> = OnceLock::new();
    let v = g(p, "z-index").unwrap_or("");
    let t = strip(v);
    if rx(&N, r"-?\d+").fullmatch(t).is_some() {
        t.parse().ok()
    } else {
        None
    }
}
