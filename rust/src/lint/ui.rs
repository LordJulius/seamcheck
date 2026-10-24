//! lint.py's checks: UI controls, Kotlin init order, id fallbacks, Elixir
//! processes, CSS layers, and `kind = "essential"`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use serde_json::{Map, Value};

use super::css::{self, Flat, OMap, Sheet};
use super::structure::{brace_tree, head_line, jsx_attrs, jsx_tree, own_text, strip_groups, Kind, Tree};
use super::{balanced_end, line, opt_str, rxs, sl, Hits};
use crate::project::{Mode, Project};
use crate::pyre::{repr, strip, take_chars, Rx};
use crate::rules::{compile, RuleErr};

type Res<T> = Result<T, RuleErr>;

pub fn lang(rel: &str) -> &'static str {
    match crate::project::ext_of(rel) {
        ".kt" | ".kts" => "kotlin",
        ".tsx" | ".jsx" => "jsx",
        ".ts" | ".js" | ".mjs" => "js",
        ".qml" => "qml",
        ".ex" | ".exs" => "elixir",
        _ => "",
    }
}

fn braces(code: &str, p: &Project, rel: &str) -> Arc<Tree> {
    p.memo(&format!("braces\0{rel}"), || brace_tree(code))
}

fn jsx(raw: &str, p: &Project, rel: &str) -> Arc<Tree> {
    p.memo(&format!("jsx\0{rel}"), || jsx_tree(raw))
}

// --- disabled-by-data ---------------------------------------------------------------

const STATUS: &str = "unread|read|is_?read|seen|is_?seen|pending|is_?pending|settled|is_?settled|status|state|expired|is_?expired|approved|denied|rejected|resolved|archived|done|is_?done|completed|dismissed|handled|opened|answered|outcome";
const BUSY: &str = "(?i)busy|loading|saving|sending|submitting|uploading|in_?flight|working|refreshing|fetching|syncing|connecting|offline|online|connected|paired|permission|allowed|editable|read_?only|can[A-Z_]";
const TARGETS: &str = r"(?i)(row|item|card|tile|entry|cell|delegate)$";

fn status_without_busy(expr: &str, o: &Map<String, Value>) -> Res<Option<String>> {
    if compile(&opt_str(o, "busy", BUSY))?.is_match(expr) {
        return Ok(None);
    }
    let status = compile(&format!("(?i)^(?:{})$", opt_str(o, "status", STATUS)))?;
    for m in rxs(0, r"\b([A-Za-z_]\w*)\s*\??\.\s*([A-Za-z_]\w*)").find_iter(expr) {
        let (owner, member) = (m.s(1), m.s(2));
        if owner.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            continue;
        }
        if status.match_start(member).is_some() {
            return Ok(Some(format!("{owner}.{member}")));
        }
    }
    Ok(None)
}

fn top_level(code: &str, tree: &Tree, k: usize) -> String {
    let n = &tree[k];
    let mut parts: Vec<&str> = Vec::new();
    let mut pos = n.start + 1;
    for &c in &n.children {
        parts.push(sl(code, pos, tree[c].start));
        pos = tree[c].end;
    }
    parts.push(sl(code, pos, n.end.saturating_sub(1)));
    parts.join(" ")
}

fn disabled_kotlin(rel: &str, code: &str, o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    if !rxs(1, r"(?<![\w.])(?:state|enabled)\s*=(?!=)").is_match(code) {
        return Ok(vec![]);
    }
    let targets = compile(&opt_str(o, "targets", TARGETS))?;
    let tree = braces(code, p, rel);
    let mut out = Vec::new();
    let callee = |pos: usize| -> String {
        let k = tree.innermost(pos, &[Kind::Paren]);
        if k < 0 {
            return String::new();
        }
        let sg = strip_groups(&tree[k as usize].name);
        rxs(2, r"([A-Za-z_]\w*)\s*$").search(&sg).map(|m| m.s(1).to_string()).unwrap_or_default()
    };
    fn expr_at(code: &str, pos: usize) -> &str {
        sl(code, pos, balanced_end(code, pos, ",)\n"))
    }
    let mut vals: HashMap<String, (usize, String)> = HashMap::new();
    for m in rxs(3, r"\b(?:val|var)\s+(\w+)\s*=(?!=)\s*").find_iter(code) {
        let e = expr_at(code, m.end());
        if e.contains("Disabled") || m.s(1) == "enabled" || m.s(1) == "isEnabled" {
            vals.insert(m.s(1).to_string(), (m.start(), e.to_string()));
        }
    }
    for m in rxs(4, r"(?<![\w.])(state|enabled)\s*=(?!=)\s*").find_iter(code) {
        if rxs(5, r"\b(?:val|var)\s+$").is_match(sl(code, m.start().saturating_sub(8), m.start())) {
            continue;
        }
        let f = callee(m.start());
        if !targets.is_match(&f) {
            continue;
        }
        let k = tree.innermost(m.start(), &[Kind::Paren]);
        let k = if k < 0 { tree.len() - 1 } else { k as usize };
        if !rxs(6, r"\bon(?:Click|LongClick|Tap|Open|Select)\s*=|\bclickable\b").is_match(&top_level(code, &tree, k)) {
            continue;
        }
        let mut e = expr_at(code, m.end()).to_string();
        let mut where_ = m.start();
        let name = strip(&e).to_string();
        if let Some((w, v)) = vals.get(&name) {
            where_ = *w;
            e = v.clone();
        }
        if m.s(1) == "state" && !e.contains("Disabled") {
            continue;
        }
        if let Some(hit) = status_without_busy(&e, o)? {
            out.push((
                line(code, where_),
                format!(
                    "{f} is disabled by the data's state ({hit}), not by work in flight: a tap on it does nothing. Let it open; show the state in the row (a dot, a subtitle)."
                ),
            ));
        }
    }
    Ok(out)
}

fn disabled_jsx(rel: &str, raw: &str, o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    let targets = compile(&opt_str(o, "targets", TARGETS))?;
    let mut out = Vec::new();
    let nodes = jsx(raw, p, rel);
    for n in nodes.iter() {
        if n.kind != Kind::El || n.name.is_empty() {
            continue;
        }
        let head = jsx_attrs(raw, n);
        let Some(m) = rxs(7, r"\bdisabled=\{").search(head) else { continue };
        let low = n.name.to_lowercase();
        let rowish = matches!(low.as_str(), "li" | "tr" | "option")
            || targets.is_match(&n.name)
            || rxs(8, r#"(?:className|role)=["'{][^"'}]*\b\w*(?:row|item|card|entry|tile|listitem|option)\b"#).is_match(head);
        if !rowish {
            continue;
        }
        let a = n.start + m.end();
        let e = sl(raw, a, balanced_end(raw, a, ""));
        if let Some(hit) = status_without_busy(e, o)? {
            out.push((
                line(raw, n.start),
                format!(
                    "<{}> is disabled by the data's state ({hit}), not by work in flight: a click on it does nothing. Let it open; show the state in the row.",
                    n.name
                ),
            ));
        }
    }
    Ok(out)
}

fn qml_type(header: &str) -> String {
    let h = strip(header);
    let Some(m) = rxs(9, r"(?:^|[\s:])((?:[A-Za-z_]\w*\.)*[A-Z]\w*)\s*$").search(h) else { return String::new() };
    if rxs(10, r"\b(function|if|else|for|while|switch|case|try|catch|return)\b").is_match(header) {
        return String::new();
    }
    m.s(1).to_string()
}

fn qml_prop(header: &str) -> String {
    rxs(11, r"\s*([a-z]\w*(?:\.\w+)*)\s*:")
        .match_start(&head_line(header))
        .map(|m| m.s(1).to_string())
        .unwrap_or_default()
}

fn disabled_qml(rel: &str, code: &str, o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    if !rxs(12, r"(?m)^\s*enabled\s*:").is_match(code) {
        return Ok(vec![]);
    }
    let targets = compile(&opt_str(o, "targets", TARGETS))?;
    let tree = braces(code, p, rel);
    let mut out = Vec::new();
    for k in 0..tree.len() {
        let n = &tree[k];
        if n.kind != Kind::Brace {
            continue;
        }
        let typ = qml_type(&n.name);
        if typ.is_empty() {
            continue;
        }
        let anc = tree.ancestors(k);
        let delegated = anc.iter().take(3).filter(|&&a| tree[a].kind == Kind::Brace).any(|&a| {
            qml_prop(&tree[a].name) == "delegate" || targets.is_match(&qml_type(&tree[a].name))
        });
        if !((targets.is_match(&typ) && typ != "Item") || ((typ == "MouseArea" || typ == "TapHandler") && delegated)) {
            continue;
        }
        let own = own_text(code, &tree, k);
        for m in rxs(13, r"(?m)^\s*enabled\s*:\s*(.+)$").find_iter(&own) {
            if let Some(hit) = status_without_busy(m.s(1), o)? {
                let needle = strip(m.group0());
                // parity: Python's str.find returns -1 when absent; _line(code, -1) counts code[:-1]
                let ln = match code.get(n.start..).and_then(|s| s.find(needle)) {
                    Some(x) => line(code, n.start + x),
                    None => code[..code.len().saturating_sub(1)].matches('\n').count() + 1,
                };
                out.push((
                    ln,
                    format!(
                        "{typ} is disabled by the data's state ({hit}), not by work in flight: a click on it does nothing. Let it open; show the state in the row."
                    ),
                ));
            }
        }
    }
    Ok(out)
}

pub fn disabled_by_data(rel: &str, raw: &str, code: &str, o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    match lang(rel) {
        "kotlin" => disabled_kotlin(rel, code, o, p),
        "jsx" => disabled_jsx(rel, raw, o, p),
        "qml" => disabled_qml(rel, code, o, p),
        _ => Ok(vec![]),
    }
}

// --- inert-control ------------------------------------------------------------------

fn handler() -> &'static Rx {
    rxs(14, r"\bon[A-Z]\w*\s*=")
}

pub fn inert_control(rel: &str, raw: &str, code: &str, _o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    let mut out = Vec::new();
    match lang(rel) {
        "jsx" => {
            let nodes = jsx(raw, p, rel);
            for (i, n) in nodes.iter().enumerate() {
                if n.kind != Kind::El || !matches!(n.name.as_str(), "button" | "input" | "select" | "textarea") {
                    continue;
                }
                let head = jsx_attrs(raw, n);
                let ln = line(raw, n.start);
                if let Some(m) = rxs(
                    15,
                    r"\b(on[A-Z]\w*)=\{\s*\(\s*\w*\s*\)\s*=>\s*(?:\{\s*\}|undefined|null|void 0)\s*\}",
                )
                .search(head)
                {
                    out.push((
                        ln,
                        format!("<{}> {} does nothing (an empty handler): wire it or remove the control.", n.name, m.s(1)),
                    ));
                    continue;
                }
                if rxs(16, r"\{\s*\.\.\.").is_match(head) || rxs(17, r"\bref=").is_match(head) {
                    continue;
                }
                if n.name == "button" {
                    if handler().is_match(head)
                        || rxs(18, r#"\btype=["']submit|\bform=|\bdisabled(?:[\s/>]|=\{?true)"#).is_match(head)
                    {
                        continue;
                    }
                    if rxs(19, r#"\btabIndex=\{?\s*["']?-1|\baria-hidden"#).is_match(head) {
                        continue;
                    }
                    if nodes
                        .ancestors(i)
                        .iter()
                        .any(|&k| nodes[k].kind == Kind::El && handler().is_match(jsx_attrs(raw, &nodes[k])))
                    {
                        continue;
                    }
                    let inner = sl(raw, n.head_end, n.end);
                    let l1 = rxs(20, r"<[^>]*>|\{[^}]*\}").sub(" ", inner);
                    let l2 = rxs(21, r"\s+").sub(" ", &l1);
                    let label = strip(&l2);
                    let label = strip(label.strip_suffix("</button>").unwrap_or(label));
                    let label = take_chars(label, 40);
                    let lab = if label.is_empty() { String::new() } else { format!(" {}", repr(label)) };
                    out.push((
                        ln,
                        format!("<button>{lab} has no handler: it does nothing when clicked. Wire it (onClick) or remove it."),
                    ));
                } else if rxs(22, r"\bdefault(?:Value|Checked)\b").is_match(head)
                    && !handler().is_match(head)
                    && !rxs(23, r"\bname=").is_match(head)
                {
                    out.push((
                        ln,
                        format!(
                            "<{}> with a default value and no onChange, name or ref: changing it saves nothing. Load and save it (controlled value + onChange).",
                            n.name
                        ),
                    ));
                }
            }
        }
        "kotlin" => {
            let mut tree: Option<Arc<Tree>> = None;
            for m in rxs(
                24,
                r"\b(onClick|onLongClick|onDoubleClick|onCheckedChange|onValueChange|onToggle|onSelect|onTap)\s*=\s*\{\s*\}|\.(clickable|combinedClickable)\s*(?:\([^()]*\))?\s*\{\s*\}",
            )
            .find_iter(code)
            {
                let what = m.get(1).or(m.get(2)).unwrap_or("");
                let t = tree.get_or_insert_with(|| braces(code, p, rel)).clone();
                let k = t.innermost(m.start(), &[Kind::Paren]);
                let call = if k >= 0 { sl(code, t[k as usize].start, t[k as usize].end) } else { "" };
                if rxs(25, r"\bindication\s*=\s*null|\benabled\s*=\s*false").is_match(call) {
                    continue;
                }
                out.push((
                    line(code, m.start()),
                    format!("{what} with an empty lambda: the control looks live and does nothing. Wire it, or drop the handler so it reads as static."),
                ));
            }
        }
        "qml" => {
            for m in rxs(26, r"\b(on(?:Clicked|Tapped|Pressed|Toggled|Activated|Triggered|DoubleClicked))\s*:\s*\{\s*\}")
                .find_iter(code)
            {
                out.push((line(code, m.start()), format!("{}: {{}} does nothing: wire it or remove the handler.", m.s(1))));
            }
        }
        _ => {}
    }
    Ok(out)
}

// --- qml-anchors-in-positioner ------------------------------------------------------

fn positioner(typ: &str) -> Option<&'static [&'static str]> {
    Some(match typ {
        "Column" => &["fill", "centerIn", "top", "bottom", "verticalCenter", "baseline"],
        "Row" => &["fill", "centerIn", "left", "right", "horizontalCenter"],
        "Grid" | "Flow" => &["fill", "centerIn", "top", "bottom", "verticalCenter", "baseline", "left", "right", "horizontalCenter"],
        "ColumnLayout" | "RowLayout" | "GridLayout" | "StackLayout" => &["*"],
        _ => return None,
    })
}

const NOT_ITEMS: &[&str] = &[
    "Repeater", "Timer", "Connections", "Binding", "Component", "QtObject", "TapHandler", "HoverHandler", "DragHandler",
    "WheelHandler", "PointHandler", "PinchHandler", "Behavior", "NumberAnimation", "SequentialAnimation",
    "ParallelAnimation", "PropertyAnimation", "ColorAnimation", "Shortcut", "ListModel", "ListElement", "State",
    "Transition", "Instantiator", "Keys", "Accessible", "Loader",
];

fn last_seg(s: &str) -> &str {
    s.rsplit('.').next().unwrap_or(s)
}

pub fn qml_anchors_in_positioner(rel: &str, _raw: &str, code: &str, _o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    if lang(rel) != "qml" || !code.contains("anchors") || !rxs(27, r"\b(?:Column|Row|Grid|Flow|\w+Layout)\s*\{").is_match(code) {
        return Ok(vec![]);
    }
    let tree = braces(code, p, rel);
    let positioned = |k: usize| -> Vec<usize> {
        let mut out = Vec::new();
        for &c in &tree[k].children {
            let ch = &tree[c];
            if ch.kind != Kind::Brace {
                continue;
            }
            let typ = qml_type(&ch.name);
            if typ.is_empty() {
                continue;
            }
            let prop = qml_prop(&ch.name);
            if last_seg(&typ) == "Repeater" && prop.is_empty() {
                for &d in &ch.children {
                    let dn = &tree[d];
                    if dn.kind == Kind::Brace && !qml_type(&dn.name).is_empty() && matches!(qml_prop(&dn.name).as_str(), "" | "delegate") {
                        out.push(d);
                    }
                }
            } else if prop.is_empty() {
                out.push(c);
            }
        }
        out
    };
    let mut out = Vec::new();
    for k in 0..tree.len() {
        if tree[k].kind != Kind::Brace {
            continue;
        }
        let full = qml_type(&tree[k].name);
        let typ = last_seg(&full);
        let Some(banned) = positioner(typ) else { continue };
        for c in positioned(k) {
            let cfull = qml_type(&tree[c].name);
            let ctyp = last_seg(&cfull);
            if NOT_ITEMS.contains(&ctyp) {
                continue;
            }
            let body = own_text(code, &tree, c);
            let mut used: BTreeSet<String> =
                rxs(28, r"(?m)^\s*anchors\.(\w+)\s*:").findall(&body).into_iter().map(String::from).collect();
            for &g in &tree[c].children {
                if tree[g].kind == Kind::Brace && head_line(&tree[g].name).starts_with("anchors") {
                    for w in rxs(29, r"(\w+)\s*:").findall(sl(code, tree[g].start, tree[g].end)) {
                        used.insert(w.to_string());
                    }
                }
            }
            let bad: Vec<String> = used
                .into_iter()
                .filter(|a| !a.ends_with("Margin") && a != "margins" && a != "alignWhenCentered")
                .filter(|a| banned.contains(&"*") || banned.contains(&a.as_str()))
                .collect();
            if !bad.is_empty() {
                let tail = if typ.ends_with("Layout") {
                    "Use Layout.* properties instead."
                } else {
                    "Size it explicitly, or use TapHandler/HoverHandler instead of a filling MouseArea."
                };
                out.push((
                    line(code, tree[c].start),
                    format!(
                        "{ctyp} inside a {typ} sets anchors.{}: the {typ} positions its children, so Qt refuses the anchor and stops laying out. {tail}",
                        bad.join(", anchors.")
                    ),
                ));
            }
        }
    }
    Ok(out)
}

// --- kotlin-init-order --------------------------------------------------------------

pub fn kotlin_init_order(rel: &str, _raw: &str, code: &str, o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    if lang(rel) != "kotlin" || !rxs(30, r"(?m)^\s*init\s*\{").is_match(code) {
        return Ok(vec![]);
    }
    let depth_limit = super::opt_int(o, "depth", 2).max(0) as usize;
    let tree = braces(code, p, rel);
    let kt_prop = rxs(
        31,
        r"^\s*(?:(?:private|internal|protected|public|override|open|final|@\w+(?:\([^)]*\))?)\s+)*(lateinit\s+)?(?:val|var)\s+(?:<[^>]*>\s*)?([A-Za-z_]\w*)\b",
    );
    let kt_fun = rxs(32, r"\bfun\s+(?:<[^>]*>\s*)?(?:[\w.]+\.)?([A-Za-z_]\w*)\s*\(");
    let mut out = Vec::new();
    for k in 0..tree.len() {
        let n = &tree[k];
        if n.kind != Kind::Brace
            || !rxs(33, r"\b(class|object)\b").is_match(&strip_groups(&n.name))
            || rxs(34, r"\b(fun|interface)\b").is_match(&head_line(&n.name))
        {
            continue;
        }
        let body_children: Vec<usize> = n.children.iter().copied().filter(|&c| tree[c].kind == Kind::Brace).collect();
        let mut props: OMap<usize> = OMap::new();
        let mut pos = n.start + 1;
        let mut inits: Vec<usize> = Vec::new();
        let mut funs: OMap<Vec<usize>> = OMap::new();
        let mut cs: Vec<Option<usize>> = body_children.iter().map(|&c| Some(c)).collect();
        cs.push(None);
        for c in cs {
            let seg_end = match c {
                Some(c) => tree[c].start,
                None => n.end.saturating_sub(1),
            };
            let seg = sl(code, pos, seg_end);
            let mut off = 0usize;
            for ln in seg.split('\n') {
                let lstart = off;
                off += ln.len() + 1;
                let Some(pm) = kt_prop.match_start(ln) else { continue };
                let rest = &ln[pm.end()..];
                if pm.get(1).is_none()
                    && !rest.contains("get()")
                    && rxs(35, r"(?<![=!<>])=(?!=)|\bby\b").is_match(rest)
                    && tree.innermost(pos + lstart + pm.span(2).unwrap().0, &[Kind::Brace, Kind::Paren]) == k as isize
                    && !props.contains(pm.s(2))
                {
                    props.insert(pm.s(2), pos + lstart);
                }
            }
            if let Some(c) = c {
                let head = head_line(&tree[c].name);
                if head == "init" {
                    inits.push(c);
                }
                if let Some(fm) = kt_fun.search(&head) {
                    let name = fm.s(1).to_string();
                    let mut v = funs.get(&name).cloned().unwrap_or_default();
                    v.push(c);
                    funs.insert(&name, v);
                }
                pos = tree[c].end;
            }
        }
        if inits.is_empty() || props.items.is_empty() {
            continue;
        }
        for &c in &inits {
            let mut seen: HashSet<usize> = HashSet::new();
            let mut frontier: Vec<(usize, Vec<String>)> = vec![(c, vec![])];
            let mut used: OMap<Vec<String>> = OMap::new();
            for _ in 0..depth_limit + 1 {
                let mut nxt = Vec::new();
                for (b, path) in &frontier {
                    if !seen.insert(*b) {
                        continue;
                    }
                    let tb = &tree[*b];
                    let text = sl(code, if *b != c { tb.hstart } else { tb.start }, tb.end);
                    let mut local: HashSet<String> = HashSet::new();
                    for w in rxs(36, r"\b(?:val|var)\s+([A-Za-z_]\w*)").findall(text) {
                        local.insert(w.to_string());
                    }
                    for w in rxs(37, r"\{\s*([A-Za-z_][\w\s,:<>?]*?)\s*->").findall(text) {
                        local.insert(w.to_string());
                    }
                    for grp in rxs(38, r"\{\s*([\w\s,]+)->").findall(text) {
                        for w in grp.split(',') {
                            local.insert(strip(w).to_string());
                        }
                    }
                    for w in rxs(39, r"([A-Za-z_]\w*)\s*:").findall(&tb.name) {
                        local.insert(w.to_string());
                    }
                    // parity: Python walks a set (hash order); first appearance here
                    let mut words: Vec<&str> = Vec::new();
                    let mut wseen: HashSet<&str> = HashSet::new();
                    for w in rxs(40, r"(?<![\w.])([A-Za-z_]\w*)\b").findall(text) {
                        if !local.contains(w) && wseen.insert(w) {
                            words.push(w);
                        }
                    }
                    for w in words {
                        if let Some(&off) = props.get(w) {
                            if off > tree[c].start && !used.contains(w) {
                                used.insert(w, path.clone());
                            }
                        }
                        if let Some(fbs) = funs.get(w) {
                            for &fb in fbs {
                                let mut np = path.clone();
                                np.push(w.to_string());
                                nxt.push((fb, np));
                            }
                        }
                    }
                }
                frontier = nxt;
            }
            let mut items: Vec<(String, Vec<String>)> = used.items.clone();
            items.sort_by_key(|(w, _)| *props.get(w).unwrap());
            for (w, path) in items {
                let via = if path.is_empty() { String::new() } else { format!(" (through {})", path.join(" -> ")) };
                out.push((
                    line(code, tree[c].start),
                    format!(
                        "init uses {w}{via}, declared below it at line {}: Kotlin initialises in source order, so {w} is still null/zero when init runs. Declare it above init.",
                        line(code, *props.get(&w).unwrap())
                    ),
                ));
            }
        }
    }
    Ok(out)
}

// --- id-kind-mix --------------------------------------------------------------------

const NO_OWNER: &[&str] = &[
    "it", "this", "self", "item", "x", "e", "data", "props", "p", "opts", "args", "params", "attrs", "payload", "body",
    "json", "obj", "m", "v", "value", "row", "entry", "record", "result", "res", "hit", "el", "element", "model", "ref",
    "node", "n", "r", "t", "a", "b", "s", "o", "d", "i", "state", "ctx",
];
const GENERIC_WORDS: &[&str] = &[
    "db", "local", "remote", "server", "client", "new", "old", "existing", "current", "selected", "active", "raw",
    "primary", "stored", "preferred", "requested", "explicit", "default", "my", "the", "to", "from", "query", "api",
    "main", "last", "next", "prev", "target", "source", "parent", "child", "root", "base", "focus", "picked", "chosen",
    "real", "stable", "external", "internal", "public",
];

fn words(name: &str) -> HashSet<String> {
    let s = name.replace('_', " ");
    rxs(41, r"[A-Z]?[a-z0-9]+|[A-Z]+(?![a-z])")
        .find_iter(&s)
        .into_iter()
        .map(|m| m.group0().to_lowercase())
        .filter(|w| !GENERIC_WORDS.contains(&w.as_str()))
        .collect()
}

/// (owner, kind, named)
fn ids(text: &str, a: usize, b: usize) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    let rx = rxs(
        42,
        r#"(?:\b([A-Za-z_]\w*)\s*(?:\??\.\s*|\[\s*["':]+))?\b([A-Za-z_]\w*?)_?(?:Id|_id|ID)\b(?!\s*\()|\b([A-Za-z_]\w*)\s*\??\.\s*id\b(?!\s*\()"#,
    );
    for m in rx.find_iter_range(text, a, b) {
        if let Some(k) = m.get(2) {
            out.push((m.s(1).to_string(), k.to_string(), true));
        } else if !NO_OWNER.contains(&m.s(3)) {
            out.push((m.s(3).to_string(), m.s(3).to_string(), false));
        }
    }
    out
}

/// The fallback operator at i (`?:`, `??`, `||`): its end.
fn op_at(b: &[u8], i: usize, lang: &str) -> Option<usize> {
    let two = |x: u8, y: u8| b.get(i) == Some(&x) && b.get(i + 1) == Some(&y);
    match lang {
        "kotlin" => two(b'?', b':').then_some(i + 2),
        "js" | "jsx" => (two(b'?', b'?') || two(b'|', b'|')).then_some(i + 2),
        "elixir" => two(b'|', b'|').then_some(i + 2),
        _ => None,
    }
}

fn left_start(text: &str, p: usize, lang: &str) -> usize {
    let b = text.as_bytes();
    let mut depth = 0i64;
    let mut i = p as isize - 1;
    while i >= 0 {
        let c = b[i as usize];
        let iu = i as usize;
        if matches!(c, b')' | b']' | b'}') {
            depth += 1;
        } else if matches!(c, b'(' | b'[' | b'{') {
            if depth == 0 {
                return iu + 1;
            }
            depth -= 1;
        } else if depth == 0 {
            if matches!(c, b',' | b';' | b'=') {
                return iu + 1;
            }
            if c == b'\n' && !strip(sl(text, iu + 1, p)).is_empty() {
                return iu + 1;
            }
            if let Some(e) = op_at(b, iu, lang) {
                if e <= p {
                    return e;
                }
            }
        }
        i -= 1;
    }
    0
}

fn right_end(text: &str, p: usize, lang: &str) -> usize {
    let b = text.as_bytes();
    let mut depth = 0i64;
    let mut i = p;
    while i < b.len() {
        let c = b[i];
        if matches!(c, b'(' | b'[' | b'{') {
            depth += 1;
        } else if matches!(c, b')' | b']' | b'}') {
            if depth == 0 {
                return i;
            }
            depth -= 1;
        } else if depth == 0 && (c == b',' || c == b';' || c == b'\n' || op_at(b, i, lang).is_some()) {
            return i;
        }
        i += 1;
    }
    i
}

fn mixes(left: &(String, String, bool), right: &(String, String, bool), named_pairs: bool) -> Option<String> {
    let (lo, lk, ln) = left;
    let (ro, rk, rn) = right;
    let _ = (lo, ro);
    if *ln && *rn {
        if !named_pairs {
            return None;
        }
        let (wl, wr) = (words(lk), words(rk));
        return if wl.is_empty() || wr.is_empty() || !wl.is_disjoint(&wr) {
            None
        } else {
            Some(format!("a {} id with a {} id", repr(lk), repr(rk)))
        };
    }
    if ln == rn {
        return None;
    }
    let (named, bare) = if *ln { (left, right) } else { (right, left) };
    if bare.0 == named.0 {
        return None;
    }
    let (wn, wb) = (words(&named.1), words(&bare.0));
    if wn.is_empty() || wb.is_empty() || !wn.is_disjoint(&wb) {
        return None;
    }
    Some(format!("a {} id with {}.id", repr(&named.1), bare.0))
}

pub fn id_kind_mix(rel: &str, _raw: &str, _code: &str, o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    let lg = lang(rel);
    if !matches!(lg, "kotlin" | "js" | "jsx" | "elixir") {
        return Ok(vec![]);
    }
    let named_pairs = crate::config::truthy(o.get("named"));
    let text = p.joined(rel, Mode::NoComment);
    let b = text.as_bytes();
    let not_fb = rxs(43, r"(?:^|[^!=<>])!(?!=)|===?|!==?|<=|>=|\$|#\{|`|\binstanceof\b|\btypeof\b");
    let mut out = Vec::new();
    let mut done: HashSet<usize> = HashSet::new();
    let mut i = 0;
    while i < b.len() {
        let Some(mend) = op_at(b, i, lg) else {
            i += 1;
            continue;
        };
        let pstart = i;
        i = mend;
        let ln = line(&text, pstart);
        if done.contains(&ln) {
            continue;
        }
        let la = left_start(&text, pstart, lg);
        let rb = right_end(&text, mend, lg);
        let (lt, rt) = (sl(&text, la, pstart), sl(&text, mend, rb));
        if not_fb.is_match(lt) || not_fb.is_match(rt) {
            continue;
        }
        let (left, right) = (ids(&text, la, pstart), ids(&text, mend, rb));
        if left.is_empty() || right.is_empty() {
            continue;
        }
        if let Some(why) = mixes(left.last().unwrap(), &right[0], named_pairs) {
            done.insert(ln);
            out.push((
                ln,
                format!(
                    "one fallback chain mixes {why}: when the first is missing the second stands in for it, and whoever reads it looks up the wrong thing (a sign-in challenge id passed as the request id said \"Request unavailable\"). Pass each id under its own name."
                ),
            ));
        }
    }
    Ok(out)
}

// --- handle-info-catch-all ----------------------------------------------------------

pub fn handle_info_catch_all(rel: &str, _raw: &str, code: &str, o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    if lang(rel) != "elixir" {
        return Ok(vec![]);
    }
    let sub = compile(&opt_str(o, "subscribe", r"\bsubscribe\(|:pg\.join\(|Registry\.register\("))?;
    let mods = p.modules(rel);
    let mut own = false;
    for m in sub.find_iter(code) {
        let ln = line(code, m.start());
        let mut f = None;
        for md in mods.iter() {
            if let Some(g) = md.func_at(ln) {
                f = Some(g);
            }
        }
        match f {
            None => {
                own = true;
                break;
            }
            Some(f) => {
                if f.kind == "private" || rxs(44, r"(init|handle_\w+|mount|join)$").match_start(&f.name).is_some() {
                    own = true;
                    break;
                }
            }
        }
    }
    if !own {
        return Ok(vec![]);
    }
    let heads = rxs(45, r"(?m)^\s*def\s+handle_info\(").find_iter(code);
    if heads.is_empty() {
        return Ok(vec![]);
    }
    for m in &heads {
        let a = m.end();
        let first = strip(sl(code, a, balanced_end(code, a, ",)")));
        let rest = sl(code, a, balanced_end(code, a, ""));
        let tail = sl(code, a + rest.len(), crate::pyre::floor_char(code, (a + rest.len() + 40).min(code.len())));
        if rxs(46, r"_?\w*").fullmatch(first).is_some() && rxs(47, r"\)\s*when\b").match_start(tail).is_none() {
            return Ok(vec![]);
        }
    }
    Ok(vec![(
        line(code, heads[0].start()),
        "subscribes to a topic, and no handle_info clause matches any message: the first message it doesn't expect (a shared topic carries others' too) is a FunctionClauseError that crashes it. End with `def handle_info(msg, state)` that logs and ignores.".into(),
    )])
}

// --- CSS ------------------------------------------------------------------------------

fn sheet(p: &Project, o: &Map<String, Value>) -> Arc<Sheet> {
    let globs = super::opt_list(o, "css", &["**/*.css"]);
    super::shared(p, &format!("css-sheet\0{}", globs.join("\0")), || {
        let files: Vec<(String, Arc<String>)> = p
            .files
            .iter()
            .filter(|f| crate::glob::match_any(f, &globs) && f.ends_with(".css"))
            .map(|f| (f.clone(), p.text(f)))
            .collect();
        css::index(&files)
    })
}

fn class_attr(head: &str) -> Vec<String> {
    let Some(m) = rxs(48, r"\bclassName=").search(head) else { return vec![] };
    let a = m.end();
    let b = head.as_bytes();
    match b.get(a) {
        Some(&q) if q == b'"' || q == b'\'' => {
            let end = head[a + 1..].find(q as char).map(|e| a + 1 + e);
            let e = match end {
                Some(e) if e > 0 => e,
                _ => head.len(),
            };
            sl(head, a + 1, e).split_whitespace().map(String::from).collect()
        }
        Some(b'{') => css::classes_in(sl(head, a, balanced_end(head, a + 1, "") + 1)),
        _ => vec![],
    }
}

/// prop -> (value, the rule (None: inline style), the class or "style")
type ElProps = OMap<(String, Option<Arc<css::Rule>>, String)>;

fn el_props(sh: &Sheet, raw: &str, n: &super::structure::Node) -> (Vec<String>, ElProps) {
    let head = jsx_attrs(raw, n);
    let cls = class_attr(head);
    let mut props: ElProps = OMap::new();
    for c in &cls {
        for (k, (val, rule)) in sh.props(c).items.iter() {
            props.insert(k, (val.clone(), Some(rule.clone()), c.clone()));
        }
    }
    if let Some(sm) = rxs(49, r"\bstyle=\{\{").search(head) {
        for m in rxs(50, r#"\b(overflow|overflowX|overflowY|position|transform)\s*:\s*["']([^"']+)["']"#)
            .find_iter(&head[sm.end()..])
        {
            let k = rxs(51, r"([A-Z])").sub_with(m.s(1), |x| format!("-{}", x.s(1).to_lowercase()));
            props.insert(&k, (m.s(2).to_string(), None, "style".into()));
        }
    }
    (cls, props)
}

fn flat_el(p: &ElProps) -> Flat {
    p.items.iter().map(|(k, v)| (k.clone(), v.0.clone())).collect()
}

fn flat_cls(p: &css::Props) -> Flat {
    p.items.iter().map(|(k, v)| (k.clone(), v.0.clone())).collect()
}

pub fn css_popup_clipped(rel: &str, raw: &str, _code: &str, o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    if lang(rel) != "jsx" {
        return Ok(vec![]);
    }
    let sh = sheet(p, o);
    let nodes = jsx(raw, p, rel);
    let mut out = Vec::new();
    let mut cache: HashMap<usize, (Vec<String>, ElProps)> = HashMap::new();
    let mut props = |k: usize| -> (Vec<String>, ElProps) {
        cache.entry(k).or_insert_with(|| el_props(&sh, raw, &nodes[k])).clone()
    };
    for (k, n) in nodes.iter().enumerate() {
        if n.kind != Kind::El || !sl(raw, n.start, n.head_end).contains("className=") {
            continue;
        }
        let (cls, pp) = props(k);
        let how = css::escapes(&flat_el(&pp));
        if how.is_empty() {
            continue;
        }
        let pop = cls
            .iter()
            .find(|c| !css::escapes(&flat_cls(&sh.props(c))).is_empty())
            .cloned()
            .unwrap_or_else(|| cls.first().cloned().unwrap_or_else(|| n.name.clone()));
        let chain: Vec<usize> = nodes.ancestors(k).into_iter().filter(|&a| nodes[a].kind == Kind::El).collect();
        let mut cb: Option<(usize, Vec<String>, String)> = None;
        for &a in &chain {
            let (acls, ap) = props(a);
            let why = css::containing_block(&flat_el(&ap));
            if !why.is_empty() {
                cb = Some((a, acls, why));
                break;
            }
        }
        let Some((cba, cbcls, cbwhy)) = cb else { continue };
        let from = chain.iter().position(|&x| x == cba).unwrap();
        for &a in &chain[from..] {
            let (_acls, ap) = props(a);
            let clip = css::clips(&flat_el(&ap));
            if clip.is_empty() {
                continue;
            }
            let src = ap.items.iter().find(|(key, _)| key.starts_with("overflow") || key == "contain").map(|(_, v)| v);
            let where_ = match src {
                Some((_, Some(r), _)) => format!(" ({}:{})", r.file, r.line),
                _ => String::new(),
            };
            let holder = match src {
                Some((_, _, c)) if c != "style" => format!(".{c}"),
                _ => format!("<{} style>", nodes[a].name),
            };
            let anchor = match cbcls.first() {
                Some(c) => format!(".{c}"),
                None => format!("<{}>", nodes[cba].name),
            };
            out.push((
                line(raw, n.start),
                format!(
                    ".{pop} opens outside its box ({how}), but {holder} ({clip}){where_} sits at or above its containing block {anchor} ({cbwhy}) and crops it to a sliver. Drop the clip, or move the popup out of the clipping box."
                ),
            ));
            break;
        }
    }
    Ok(out)
}

pub fn css_overlay_order(rel: &str, _raw: &str, _code: &str, o: &Map<String, Value>, p: &Project) -> Res<Hits> {
    if !rel.ends_with(".css") {
        return Ok(vec![]);
    }
    let sh = sheet(p, o);
    let over = compile(&opt_str(o, "overlays", r"(?i)overlay|modal|dialog|scrim|backdrop|lightbox"))?;
    let shell_rx =
        compile(&opt_str(o, "shell", r"(?i)(^|-)(dock|rail|header|footer|toolbar|topbar|tabbar|nav|hub)(-|$)"))?;
    let above: Option<Vec<String>> = o.get("above").map(|_| super::opt_list(o, "above", &[]));
    let min_z = super::opt_int(o, "min_shell_z", 10);
    // (class, z, props, flat props) of every positioned layer: once per sheet
    let globs = super::opt_list(o, "css", &["**/*.css"]);
    let layers = super::shared(p, &format!("css-layers\0{}", globs.join("\0")), || {
        let mut layers: Vec<(String, i64, Arc<css::Props>, Flat)> = Vec::new();
        for (c, _) in sh.base.iter() {
            let pp = sh.props(c);
            let flat = flat_cls(&pp);
            let Some(z) = css::z_index(&flat) else { continue };
            let pos = pp.get("position").map(|x| x.0.clone()).unwrap_or_default();
            if pos != "fixed" && pos != "absolute" {
                continue;
            }
            layers.push((c.clone(), z, pp, flat));
        }
        layers
    });
    let shell: Vec<(String, i64)> = match &above {
        Some(a) if crate::config::truthy(o.get("above")) => {
            layers.iter().filter(|(c, _, _, _)| a.contains(c)).map(|(c, z, _, _)| (c.clone(), *z)).collect()
        }
        _ => layers
            .iter()
            .filter(|(c, z, _, flat)| shell_rx.is_match(c) && *z >= min_z && !css::covers(flat))
            .map(|(c, z, _, _)| (c.clone(), *z))
            .collect(),
    };
    if shell.is_empty() {
        return Ok(vec![]);
    }
    let mut top = &shell[0];
    for s in &shell[1..] {
        if s.1 > top.1 {
            top = s;
        }
    }
    let (top_c, top_z) = top.clone();
    let mut out = Vec::new();
    for (c, z, pp, flat) in layers.iter() {
        if !over.is_match(c) || !css::covers(flat) || pp.get("pointer-events").map(|x| x.0.as_str()) == Some("none") {
            continue;
        }
        let mut under: Vec<(String, i64)> = shell.iter().filter(|(sc, sz)| sz > z && sc != c).cloned().collect();
        if under.is_empty() {
            continue;
        }
        let rule = &pp.get("z-index").unwrap().1;
        if rule.file != rel {
            continue;
        }
        under.sort_by_key(|(_, sz)| -sz);
        let names: Vec<String> = under.iter().take(3).map(|(sc, sz)| format!(".{sc} ({sz})")).collect();
        out.push((
            rule.line,
            format!(
                ".{c} covers the screen at z-index {z}, under {}: it opens behind them (the approval drew under the chat dock). Raise it above {top_z} (.{top_c}).",
                names.join(", ")
            ),
        ));
    }
    Ok(out)
}

// --- elixir-rescue-debug --------------------------------------------------------------

pub fn elixir_rescue_debug(rel: &str, _raw: &str, code: &str, _o: &Map<String, Value>, _p: &Project) -> Res<Hits> {
    if !(rel.ends_with(".ex") || rel.ends_with(".exs")) || !code.contains("Logger.debug") {
        return Ok(vec![]);
    }
    let loud = rxs(
        52,
        r"\bLogger\.(?:warning|warn|error|info|notice|critical|alert|emergency)\b|\b(?:re)?raise\b|:telemetry\.|\{:error\b|\bthrow\b|\bexit\(",
    );
    let lines: Vec<&str> = code.split('\n').collect();
    let indent = |s: &str| s.len() - s.trim_start().len();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let Some(m) = rxs(53, r"^(\s*)(rescue|catch)\b").match_start(l) else { continue };
        let ind = m.s(1).len();
        let mut body: Vec<(usize, &str)> = Vec::new();
        for (j, nxt) in lines.iter().enumerate().skip(i + 1) {
            if !strip(nxt).is_empty() && indent(nxt) <= ind {
                break;
            }
            body.push((j, nxt));
        }
        let text = body.iter().map(|(_, b)| *b).collect::<Vec<_>>().join("\n");
        if text.contains("Logger.debug") && !loud.is_match(&text) {
            let ln = body.iter().find(|(_, b)| b.contains("Logger.debug")).unwrap().0;
            out.push((
                ln + 1,
                "this rescue only logs at debug, which production drops: the failure is invisible (Spotify's last-device save raised on every poll and nobody saw it). Log at warning, or return the error.".into(),
            ));
        }
    }
    Ok(out)
}

// --- essential controls -------------------------------------------------------------

const MENUS: &str = r"(?i)\b(?:\w*Menu|Popover|Popup|Dropdown\w*|Dialog|Drawer|BottomSheet|ModalBottomSheet|Tooltip)\b|\b(?:v-menu|menu|popover|dropdown)\b";

fn disabled_false(spec: &Map<String, Value>) -> bool {
    matches!(spec.get("disabled"), Some(Value::Bool(false)))
}

fn when_matches(spec: &Map<String, Value>, text: &str) -> Res<bool> {
    match spec.get("when") {
        Some(Value::String(w)) if !w.is_empty() => Ok(compile(w)?.is_match(text)),
        _ => Ok(false),
    }
}

pub fn essential(p: &Project, r: &Map<String, Value>) -> Res<Vec<(String, usize, String)>> {
    let mut out = Vec::new();
    let menus = compile(&crate::config::s(r, "menus").map(String::from).unwrap_or_else(|| MENUS.into()))?;
    let inn = match crate::rules::need(r, "in")? {
        Value::Array(a) => a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect::<Vec<_>>(),
        Value::String(s) => s.chars().map(|c| c.to_string()).collect(),
        _ => vec![],
    };
    for rel in inn {
        if !p.path(&rel).is_file() {
            out.push((rel.clone(), 0, format!("essential-controls file is gone: {rel}")));
            continue;
        }
        let raw = p.text(&rel);
        let lg = lang(&rel);
        let bar_src = crate::rules::need_str(r, "bar")?;
        let Some(bm) = compile(bar_src)?.search(&raw) else {
            out.push((rel.clone(), 0, format!("bar {} not found in {rel}", repr(bar_src))));
            continue;
        };
        let (bs, be) = (bm.start(), bm.end());
        let controls = crate::rules::need(r, "controls")?.as_object().ok_or_else(|| RuleErr::Key("controls".into()))?;
        for (name, spec) in controls {
            let spec: Map<String, Value> = match spec {
                Value::String(s) => {
                    let mut m = Map::new();
                    m.insert("marker".into(), Value::String(s.clone()));
                    m
                }
                Value::Object(o) => o.clone(),
                _ => Map::new(),
            };
            let marker = crate::rules::need_str(&spec, "marker")?.to_string();
            let hits = compile(&marker)?.find_iter(&raw);
            if hits.is_empty() {
                out.push((rel.clone(), line(&raw, bs), format!("essential control {} is gone (no {})", repr(name), repr(&marker))));
                continue;
            }
            let mut msgs: Vec<(usize, String)> = Vec::new();
            let mut broke = false;
            for hm in &hits {
                let why = if lg == "jsx" {
                    essential_jsx(&raw, bs, hm.start(), &spec, &menus)?
                } else {
                    let (code, _) = crate::scan::blank(&raw, if lg == "kotlin" { "kotlin" } else { "js" }, false);
                    essential_braces(&code, bs, be, hm.start(), &spec, &menus, lg)?
                };
                if why.is_empty() {
                    broke = true;
                    break;
                }
                msgs.push((line(&raw, hm.start()), why));
            }
            if !broke {
                let (ln, why) = &msgs[0];
                out.push((
                    rel.clone(),
                    *ln,
                    format!(
                        "essential control {} {why}: it must sit on the bar unconditionally, or it can't be reached when it matters (an approval waiting mid-turn).",
                        repr(name)
                    ),
                ));
            }
        }
    }
    Ok(out)
}

fn essential_jsx(raw: &str, bpos: usize, hpos: usize, spec: &Map<String, Value>, menus: &Rx) -> Res<String> {
    let nodes = jsx_tree(raw);
    let bar = nodes.innermost(bpos, &[Kind::El]);
    let ctl = nodes.innermost(hpos, &[Kind::El]);
    if bar < 0 || ctl < 0 {
        return Ok("is not inside a JSX element".into());
    }
    let (bar, ctl) = (bar as usize, ctl as usize);
    let head = jsx_attrs(raw, &nodes[ctl]);
    if disabled_false(spec) && rxs(54, r"\bdisabled(?:=|[\s/>])").is_match(head) {
        let g = rxs(55, r"\bdisabled(?:=\{[^}]*\}|[^\s/>]*)").search(head).map(|m| m.group0().to_string()).unwrap_or_default();
        return Ok(format!("can be disabled ({g})"));
    }
    let chain = nodes.ancestors(ctl);
    let bi = chain.iter().position(|&x| x == bar);
    if bi.is_none() && bar != ctl {
        return Ok("is not inside the bar".into());
    }
    let upto: &[usize] = match bi {
        Some(i) => &chain[..i],
        None => &[],
    };
    for &a in upto {
        let n = &nodes[a];
        if n.kind == Kind::Expr {
            let end = if n.children.is_empty() { nodes[ctl].start } else { nodes[n.children[0]].start };
            let cond = sl(raw, n.start + 1, end);
            let cond = rxs(56, r"\s+").sub(" ", cond);
            let cond = take_chars(strip(&cond), 60).to_string();
            if when_matches(spec, &cond)? {
                continue;
            }
            return Ok(format!("renders only when {{{cond} ...}}"));
        }
        if n.kind == Kind::El && (menus.is_match(&n.name) || menus.is_match(jsx_attrs(raw, n))) {
            let attrs = jsx_attrs(raw, n);
            let rest = attrs.get(n.name.len() + 1..).unwrap_or("");
            return Ok(format!("is inside a menu (<{} {}...)", n.name, strip(take_chars(rest, 50))));
        }
    }
    Ok(String::new())
}

fn essential_braces(
    code: &str,
    bstart: usize,
    bend: usize,
    hpos: usize,
    spec: &Map<String, Value>,
    menus: &Rx,
    lg: &str,
) -> Res<String> {
    let nodes = brace_tree(code);
    let opens = |start: usize| -> bool {
        let mut rest = String::new();
        let mut depth = 0i64;
        for ch in sl(code, bend, start).chars() {
            if ch == '(' {
                depth += 1;
            } else if ch == ')' {
                if depth == 0 {
                    rest.clear();
                } else {
                    depth -= 1;
                }
            } else if depth == 0 {
                rest.push(ch);
            }
        }
        !rest.trim_end().contains('\n')
    };
    let heads: Vec<usize> = (0..nodes.len())
        .filter(|&k| {
            let n = &nodes[k];
            n.kind == Kind::Brace && n.hstart <= bstart && bstart < n.start && opens(n.start)
        })
        .collect();
    let bar: isize = if heads.is_empty() {
        nodes.innermost(bstart, &[Kind::Brace])
    } else {
        let mut best = heads[0];
        for &k in &heads[1..] {
            if nodes[k].start - nodes[k].hstart < nodes[best].start - nodes[best].hstart {
                best = k;
            }
        }
        best as isize
    };
    let ctl = nodes.innermost(hpos, &[Kind::Brace, Kind::Paren]);
    if bar < 0 || ctl < 0 {
        return Ok("is not inside the bar".into());
    }
    let (bar, ctl) = (bar as usize, ctl as usize);
    let mut chain = vec![ctl];
    chain.extend(nodes.ancestors(ctl));
    let Some(bi) = chain.iter().position(|&x| x == bar) else { return Ok("is not inside the bar".into()) };
    let kt_branch = rxs(57, r"^(?:if\b|else\b|when\b)|->$|\?\.let$|\.let$|\.also$|AnimatedVisibility\b");
    for &a in &chain[..bi] {
        let n = &nodes[a];
        let head = head_line(&n.name);
        if lg == "qml" {
            let body = if n.kind == Kind::Brace { own_text(code, &nodes, a) } else { String::new() };
            if let Some(vm) = rxs(58, r"(?m)^\s*visible\s*:\s*(.+)$").search(&body) {
                let v = strip(vm.s(1));
                if v != "true" && !when_matches(spec, vm.s(1))? {
                    return Ok(format!("renders only when visible: {}", take_chars(v, 60)));
                }
            }
            if disabled_false(spec) && rxs(59, r"(?m)^\s*enabled\s*:").is_match(&body) {
                return Ok("can be disabled (enabled: ...)".into());
            }
            if menus.is_match(&head) || rxs(60, r"(Loader|Repeater)\b").match_start(&head).is_some() {
                return Ok(format!("is inside {}", take_chars(&head, 50)));
            }
            continue;
        }
        if kt_branch.is_match(&head) {
            if when_matches(spec, &n.name)? {
                continue;
            }
            let cond = rxs(61, r"\s+").sub(" ", strip(&n.name));
            return Ok(format!("renders only when {}", take_chars(&cond, 60)));
        }
        if menus.is_match(&head) {
            return Ok(format!("is inside a menu ({})", take_chars(&head, 50)));
        }
        if n.kind == Kind::Paren && disabled_false(spec) {
            let args = sl(code, n.start, n.end);
            if rxs(62, r"\benabled\s*=(?!=)").is_match(args) || args.contains("Disabled") {
                return Ok("can be disabled (enabled = ...)".into());
            }
        }
    }
    Ok(String::new())
}
