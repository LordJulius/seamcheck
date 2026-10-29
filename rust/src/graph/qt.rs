//! Qt / QML passes core runs once imports are resolved: D-Bus proxies stand
//! for the class they call (lang_other.qt_dbus_interfaces), what each QML
//! page's untyped properties are handed (qml_page_props, qml_delegate_roles,
//! _cpp_model_roles) and the objects C++ hands QML by name (core._qml_context).

use std::collections::{BTreeSet, HashMap, HashSet};

use indexmap::IndexMap;

use crate::graph::infer::{esc, re, rxd};
use crate::graph::Graph;
use crate::lang::chains::expression;
use crate::lang::lambdas::matching;

fn stem(file: &str) -> String {
    let base = file.rsplit('/').next().unwrap_or(file);
    base[..base.len().saturating_sub(4)].to_string()
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn chain_of(code: &str, pos: usize) -> Vec<String> {
    expression(code, pos, 40, Default::default())
}

/// `{ a: x, b: f(y) }` / an object block's own `name: value` lines: name -> value text.
pub fn qml_object_props(code: &str, brace: usize) -> IndexMap<String, String> {
    let mut out = IndexMap::new();
    let close = matching(code, brace);
    if close < 0 {
        return out;
    }
    let inner = &code[brace + 1..close as usize];
    let (mut depth, mut start) = (0i32, 0usize);
    let mut parts: Vec<&str> = Vec::new();
    for (k, ch) in inner.char_indices() {
        if "([{".contains(ch) {
            depth += 1;
        } else if ")]}".contains(ch) {
            depth -= 1;
        } else if depth == 0 && ",;\n".contains(ch) {
            parts.push(&inner[start..k]);
            start = k + 1;
        }
    }
    parts.push(&inner[start..]);
    for part in parts {
        if let Some(pm) = re!(r"(?s)\s*([a-z_]\w*)\s*:\s*(.+?)\s*$").match_start(part) {
            out.insert(pm.s(1).to_string(), pm.s(2).to_string());
        }
    }
    out
}

/// Qt D-Bus: `class Device { Q_CLASSINFO("D-Bus Interface", "org.kde.kdeconnect.device") }`
/// is what a proxy generated from that interface calls across the bus: the
/// generated name stands for the class.
pub fn qt_dbus_interfaces(g: &mut Graph) {
    const CPP: &[&str] = &[".h", ".hpp", ".hh", ".cpp", ".cc", ".cxx"];
    let mut adds: Vec<(String, String)> = Vec::new();
    let names: Vec<String> = g.modules.keys().cloned().collect();
    for mn in &names {
        let m = &g.modules[mn];
        if !CPP.iter().any(|e| m.file.ends_with(e)) {
            continue;
        }
        let text = m.raw.join("\n");
        if !text.contains("D-Bus Interface") {
            continue;
        }
        for cm in re!(r#"Q_CLASSINFO\(\s*"D-Bus Interface"\s*,\s*"([\w.]+)"\s*\)"#).find_iter(&text) {
            let cls: Vec<String> =
                re!(r"(?m)^\s*class\s+(?:\w+_EXPORT\s+)?([A-Za-z_]\w*)\b[^;]*$").find_iter(&text[..cm.start()]).iter().map(|c| c.s(1).to_string()).collect();
            let Some(last) = cls.last() else { continue };
            let generated: String = cm
                .s(1)
                .split('.')
                .map(|p| {
                    let mut c = p.chars();
                    match c.next() {
                        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                        None => String::new(),
                    }
                })
                .collect::<String>()
                + "Interface";
            let mut owners: Vec<String> = g
                .modules
                .values()
                .filter(|o| o.decls.contains(last) && CPP.iter().any(|e| o.file.ends_with(e)))
                .map(|o| o.name.clone())
                .collect();
            if owners.is_empty() {
                owners.push(mn.clone());
            }
            for o in owners {
                adds.push((o, generated.clone()));
            }
        }
    }
    for (o, gen) in adds {
        let om = g.modules.get_mut(&o).unwrap();
        if !om.decls.contains(&gen) {
            om.decls.push(gen);
        }
    }
}

/// A C++ item model's roles as QML sees them: `roleNames()` names each role
/// and `data()` returns its value: class module -> {role name: "type:T"}.
fn cpp_model_roles(g: &Graph) -> IndexMap<String, IndexMap<String, String>> {
    let mut out: IndexMap<String, IndexMap<String, String>> = IndexMap::new();
    for id in g.by_name.get("roleNames").map(|v| v.as_slice()).unwrap_or(&[]) {
        let Some(f) = g.func(id) else { continue };
        if ![".cpp", ".cc", ".cxx", ".h", ".hpp"].iter().any(|e| f.file.ends_with(e)) {
            continue;
        }
        let raw = &g.modules[&f.module].raw;
        let src = raw[(f.start - 1).min(raw.len())..f.end.min(raw.len())].join("\n"); // (the role names are strings: raw text)
        let mut names: IndexMap<String, String> = IndexMap::new();
        for c in re!(r#"\b(\w+Role)\b\s*,\s*(?:QByteArrayLiteral\()?\s*"(\w+)""#).find_iter(&src) {
            names.insert(c.s(1).to_string(), c.s(2).to_string());
        }
        for c in re!(r#"\[\s*(\w+Role)\s*\]\s*=\s*(?:QByteArrayLiteral\()?\s*"(\w+)""#).find_iter(&src) {
            names.insert(c.s(1).to_string(), c.s(2).to_string());
        }
        let Some(data) = g.func(&format!("{}.data", f.module)) else { continue };
        if names.is_empty() {
            continue;
        }
        let mut roles: IndexMap<String, String> = IndexMap::new();
        for (en, qml_name) in &names {
            let Some(cm) = rxd(&format!(
                r"\bcase\s+(?:\w+::)*{}\s*:\s*\{{?\s*return\s+(?:QVariant::fromValue\s*(?:<[^>]*>)?\s*\(\s*)?([a-z_]\w*)\s*\)?\s*;",
                esc(en)
            ))
            .search(&data.code) else {
                continue;
            };
            if let Some(dm) = rxd(&format!(r"\b([A-Z][\w:]*)\s*\*\s*{}\s*=", esc(cm.s(1)))).search(&data.code) {
                roles.insert(qml_name.clone(), format!("type:{}", dm.s(1)));
            }
        }
        if !roles.is_empty() {
            out.insert(f.module.clone(), roles);
        }
    }
    out
}

type Delegates = HashMap<String, Vec<(usize, usize, IndexMap<String, String>)>>;

/// A delegate's bare role names (`device` in a ListView delegate whose model is
/// `DevicesSortProxyModel { sourceModel: devicesModel }` over a C++ DevicesModel):
/// QML module -> [(first line, last line, {role: held})], also bound on the
/// delegate's own handlers.
fn qml_delegate_roles(g: &mut Graph) -> Delegates {
    let roles = cpp_model_roles(g);
    if roles.is_empty() {
        return Delegates::new();
    }
    let mut by_class: HashMap<String, IndexMap<String, String>> = HashMap::new();
    for (mn, rs) in &roles {
        let cls = if mn.contains('.') { mn.rsplit('@').last().unwrap_or(mn).rsplit('.').next().unwrap().to_string() } else { basename(mn).to_string() };
        let _ = &cls;
        let cls = if mn.contains('.') {
            let head = mn.rsplit_once('@').map(|x| x.0).unwrap_or(mn);
            head.rsplit('.').next().unwrap().to_string()
        } else {
            basename(mn).to_string()
        };
        by_class.entry(cls).or_default().extend(rs.clone());
        if let Some(m_) = g.modules.get(mn) {
            for d in &m_.decls {
                by_class.entry(d.clone()).or_default().extend(rs.clone());
            }
        }
    }
    let mut out: Delegates = HashMap::new();
    let mut fbound: Vec<(String, usize, IndexMap<String, String>)> = Vec::new();
    for m in g.modules.values() {
        if !m.file.ends_with(".qml") || !m.owner.is_empty() {
            continue;
        }
        let code = m.code.join("\n");
        for vm in re!(r"\bmodel\s*:\s*").find_iter(&code) {
            let k = vm.end();
            let mut src = String::new();
            let mut cls = String::new();
            if let Some(om) = re!(r"([A-Z][\w.]*)\s*\{").match_start(&code[k..]) {
                // `model: DevicesSortProxyModel { sourceModel: devicesModel }`
                let props = qml_object_props(&code, k + om.end() - 1);
                src = props.get("sourceModel").cloned().unwrap_or_default();
                cls = if by_class.contains_key(om.s(1)) && src.is_empty() { om.s(1).to_string() } else { String::new() };
            } else {
                let eol = code[k..].find('\n').map(|x| x + k).unwrap_or(code.len());
                if let Some(im) = re!(r"([a-z_]\w*)\s*$").match_start(&code[k..eol]) {
                    src = im.s(1).to_string();
                }
            }
            if !src.is_empty() && cls.is_empty() {
                let held = m.bound.get(&src).cloned().unwrap_or_default();
                cls = held.strip_prefix("type:").unwrap_or("").to_string();
            }
            let rs = if cls.is_empty() { None } else { by_class.get(cls.rsplit('.').next().unwrap()) };
            let Some(rs) = rs.filter(|r| !r.is_empty()) else { continue };
            // the delegate of the object holding this `model:` (the view)
            let b = code.as_bytes();
            let (mut depth, mut a) = (0i32, vm.start());
            while a > 0 {
                a -= 1;
                if b[a] == b'}' {
                    depth += 1;
                } else if b[a] == b'{' {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
            }
            let close = matching(&code, a);
            let mut dm = None; // the view's own `delegate:` (not a `section { delegate: ... }` inside it)
            if close > 0 {
                for cand in re!(r"\bdelegate\s*:\s*[A-Z][\w.]*\s*\{").find_iter(&code[a..close as usize]) {
                    let seg = &code[a + 1..a + cand.start()];
                    if seg.matches('{').count() == seg.matches('}').count() {
                        dm = Some(cand.end());
                        break;
                    }
                }
            }
            let Some(dend) = dm else { continue };
            let d_open = a + dend - 1;
            let d_close = matching(&code, d_open);
            let l0 = code[..d_open].matches('\n').count() + 1;
            let l1 = code[..d_close.max(0) as usize].matches('\n').count() + 1;
            out.entry(m.name.clone()).or_default().push((l0, l1, rs.clone()));
            for (fi, f) in m.funcs.iter().enumerate() {
                if l0 <= f.start && f.end <= l1 {
                    fbound.push((m.name.clone(), fi, rs.clone()));
                }
            }
        }
    }
    for (mn, fi, rs) in fbound {
        let f = &mut g.modules.get_mut(&mn).unwrap().funcs[fi];
        for (r, held) in rs {
            f.bound.entry(r).or_insert(held);
        }
    }
    out
}

/// `pageStack.push(Qt.resolvedUrl("mpris.qml"), {pluginInterface: obj})`,
/// `push(deviceComp, {currentDevice: device})`, `DevicePage { currentDevice: d }`:
/// a component's untyped property (`property QtObject pluginInterface`) holds
/// what every such site hands it, evaluated where it is handed.
pub fn qml_page_props(g: &mut Graph) {
    let delegates = qml_delegate_roles(g);
    let qml: Vec<String> = g.modules.values().filter(|m| m.file.ends_with(".qml") && m.owner.is_empty()).map(|m| m.name.clone()).collect();
    let text: HashMap<String, String> = qml.iter().map(|n| (n.clone(), g.modules[n].code.join("\n"))).collect(); // (blanked: strings are spaces)
    let raw: HashMap<String, String> = qml.iter().map(|n| (n.clone(), g.modules[n].raw.join("\n"))).collect();
    let mut by_stem: HashMap<String, Vec<String>> = HashMap::new();
    for n in &qml {
        by_stem.entry(stem(&g.modules[n].file)).or_default().push(n.clone());
    }
    let mut handed: IndexMap<(String, String), Vec<String>> = IndexMap::new();
    let enclosing_function = |code: &str, pos: usize| -> usize {
        // where the innermost function / handler / arrow body holding `pos` starts
        let b = code.as_bytes();
        let mut depth = 0i32;
        let mut k = pos.min(b.len());
        while k > 0 {
            k -= 1;
            if b[k] == b'}' {
                depth += 1;
            } else if b[k] == b'{' {
                if depth > 0 {
                    depth -= 1;
                } else {
                    let lo = crate::pyre::floor_char(code, k.saturating_sub(200));
                    if re!(r"(?:\bfunction\b[^{};]*|=>\s*|\)\s*|\bon[A-Z]\w*\s*:\s*)$").search(&code[lo..k]).is_some() {
                        return k;
                    }
                }
            }
        }
        0
    };
    let role_at = |mn: &str, pos: usize, name: &str| -> String {
        let t = &text[mn];
        let ln = t[..pos.min(t.len())].matches('\n').count() + 1;
        for (l0, l1, rs) in delegates.get(mn).map(|v| v.as_slice()).unwrap_or(&[]) {
            if *l0 <= ln && ln <= *l1 {
                if let Some(r) = rs.get(name) {
                    return r.clone();
                }
            }
        }
        String::new()
    };
    let mut hand = |g: &Graph, target: &str, props: &IndexMap<String, String>, where_: &str, subst: &IndexMap<String, String>, at: usize| {
        let Some(tms) = by_stem.get(target) else { return };
        if tms.len() != 1 {
            return;
        }
        let tm = &g.modules[&tms[0]];
        for (k, v) in props {
            if tm.bound.get(k).map(|s| s.as_str()).unwrap_or("qmlvar:") != "qmlvar:" && handed.get(&(tm.name.clone(), k.clone())).map_or(true, |x| x.is_empty()) {
                continue; // (a declared type says more)
            }
            let mut ch = chain_of(&format!("{v}\n"), 0);
            if ch.len() == 1 && !ch[0].ends_with("()") {
                let code_ = &text[where_];
                let ef = enclosing_function(code_, at);
                let decls = rxd(&format!(r"\b(?:var|let|const)\s+{}\s*=", esc(&ch[0]))).find_iter(&code_[ef..at.min(code_.len()).max(ef)]);
                if let Some(last) = decls.last() {
                    let c2 = chain_of(code_, ef + last.end());
                    if !c2.is_empty() {
                        ch = c2;
                    }
                }
            }
            if ch.len() == 1 {
                let role = role_at(where_, at, &ch[0]);
                if !role.is_empty() {
                    handed.entry((tm.name.clone(), k.clone())).or_default().push(format!("{where_}\x1b{role}")); // a delegate's model role
                    continue;
                }
            }
            if let Some(s) = ch.first().and_then(|c| subst.get(c)) {
                // the instance's own value of that property
                let sub = chain_of(&format!("{s}\n"), 0);
                ch = if sub.is_empty() { vec![] } else { [sub, ch[1..].to_vec()].concat() };
            }
            let tail = if ch.is_empty() { "?".to_string() } else { format!("expr:{}", ch.join("\x1f")) };
            handed.entry((tm.name.clone(), k.clone())).or_default().push(format!("{where_}\x1b{tail}"));
        }
    };
    let mut comp_ids: HashMap<String, HashMap<String, String>> = HashMap::new();
    for n in &qml {
        let mut ids = HashMap::new();
        let code = &text[n];
        for cm in re!(r"\bComponent\s*\{").find_iter(code) {
            let props_ = qml_object_props(code, cm.end() - 1);
            let close = matching(code, cm.end() - 1);
            let end = if close >= 0 { close as usize } else { code.len().saturating_sub(1) };
            let inner = if cm.end() <= end { re!(r"\b([A-Z]\w*)\s*\{").search(&code[cm.end()..end]).map(|c| c.s(1).to_string()) } else { None };
            if let (Some(id), Some(inner)) = (props_.get("id"), inner) {
                ids.insert(id.clone(), inner);
            }
        }
        comp_ids.insert(n.clone(), ids);
    }
    let push_re = re!(
        r#"\b(?:push|replace|pushDialogLayer|push\w*|setSource|createObject)\s*\(\s*(?:Qt\.resolvedUrl\(\s*)?(?:"([^"]+?)\.qml"|'([^']+?)\.qml'|([A-Za-z_]\w*))\s*\)?\s*,\s*\{"#
    );
    let empty: IndexMap<String, String> = IndexMap::new();
    for n in &qml {
        let code = &text[n];
        let own_props: HashSet<String> = re!(r"\bproperty\s+[\w.<>]+\s+([a-z_]\w*)").find_iter(code).iter().map(|c| c.s(1).to_string()).collect();
        for pm in push_re.find_iter(&raw[n]) {
            let props = qml_object_props(code, pm.end() - 1);
            let name = pm.get(1).filter(|s| !s.is_empty()).or_else(|| pm.get(2).filter(|s| !s.is_empty()));
            if let Some(name) = name {
                hand(g, basename(name), &props, n, &empty, pm.start());
            } else if let Some(c) = pm.get(3).and_then(|x| comp_ids[n].get(x)) {
                hand(g, c, &props, n, &empty, pm.start());
            } else if own_props.contains(pm.s(3)) {
                // `push(Qt.resolvedUrl(component), {...})`: each instance's page
                let st = stem(&g.modules[n].file);
                for im in &qml {
                    for om in rxd(&format!(r"(?<![\w.]){}\s*\{{", esc(&st))).find_iter(&text[im]) {
                        let inst = qml_object_props(&raw[im], om.end() - 1);
                        let page = inst.get(pm.s(3)).and_then(|v| re!(r#"["']([^"']+?)\.qml["']$"#).match_start(v).map(|c| c.s(1).to_string()));
                        if let Some(page) = page {
                            hand(g, basename(&page), &props, n, &inst, pm.start());
                        }
                    }
                }
            }
        }
        for om in re!(r"(?<![\w.])([A-Z]\w*)\s*\{").find_iter(code) {
            if by_stem.contains_key(om.s(1)) && om.s(1) != stem(&g.modules[n].file) {
                let inst: IndexMap<String, String> = qml_object_props(code, om.end() - 1).into_iter().filter(|(k, _)| k != "id").collect();
                hand(g, om.s(1), &inst, n, &empty, om.start());
            }
        }
    }
    let upd: Vec<((String, String), Vec<String>)> = handed.into_iter().collect();
    for ((mn, k), alts) in upd {
        let set: BTreeSet<String> = alts.into_iter().collect();
        g.modules.get_mut(&mn).unwrap().bound.insert(k, format!("oneof:{}", set.into_iter().collect::<Vec<_>>().join("\x1c")));
    }
}

/// Objects C++ hands to QML by name (`setContextProperty("continuity",
/// &continuity)`), typed by the variable's declaration in that file
/// (`ContinuityClient continuity;`): every QML file's `continuity.refresh()`
/// is then a call on ContinuityClient, not a guess.
pub fn qml_context(g: &mut Graph) {
    let mut names: IndexMap<String, Vec<String>> = IndexMap::new();
    let mut qnames: Vec<(String, String)> = Vec::new();
    let files: Vec<String> = g.by_file.keys().cloned().collect();
    for rel in &files {
        let mods = &g.by_file[rel];
        let is_cpp = [".h", ".hpp", ".hh", ".cpp", ".cc", ".cxx", ".c"].iter().any(|e| rel.ends_with(e));
        if is_cpp && !mods.is_empty() && g.modules[&mods[0]].raw.join("\n").contains("QML_NAMED_ELEMENT") {
            // `class FooForeign { QML_NAMED_ELEMENT(Foo) QML_FOREIGN(FooImpl) }`: QML's `Foo` is FooImpl
            let raw = g.modules[&mods[0]].raw.join("\n");
            let parts = re!(r"\b(?:class|struct)\s+(?=\w+[^;]*\{)").split(&raw);
            for body in parts.iter().skip(1) {
                let cls = re!(r"(?:\w+\s+)*?(\w+)\s*(?::|\{)").match_start(body);
                let named = re!(r"\bQML_NAMED_ELEMENT\s*\(\s*(\w+)\s*\)").search(body);
                if let (Some(cls), Some(named)) = (cls, named) {
                    let foreign = re!(r"\bQML_FOREIGN\s*\(\s*([\w:]+)\s*\)").search(body);
                    let target = foreign.map(|f| f.s(1).to_string()).unwrap_or_else(|| cls.s(1).to_string());
                    qnames.push((named.s(1).to_string(), target.rsplit("::").next().unwrap().to_string()));
                }
            }
        }
        if ![".cpp", ".cc", ".cxx"].iter().any(|e| rel.ends_with(e)) || mods.is_empty() {
            continue;
        }
        let raw = g.modules[&mods[0]].raw.join("\n");
        // `qmlRegisterType<ConversationModel>("uri", 1, 0, "ConversationModel")`: a C++ class under a QML name
        for c in re!(r#"qmlRegister\w*Type<\s*([A-Za-z_][\w:]*)\s*>\s*\([^;]*?"(\w+)"\s*[,)]"#).find_iter(&raw) {
            qnames.push((c.s(2).to_string(), c.s(1).rsplit("::").next().unwrap().to_string()));
        }
        if !raw.contains("setContextProperty") {
            continue;
        }
        for c in re!(r#"setContextProperty\(\s*(?:QStringLiteral\()?\s*"(\w+)"\s*\)?\s*,\s*&?\s*(\w+)"#).find_iter(&raw) {
            let (name, var) = (c.s(1).to_string(), c.s(2).to_string());
            if var == "new" {
                // `setContextProperty("X", new Foo(...))`
                if let Some(nm) = rxd(&format!(r#""{}"\s*\)?\s*,\s*new\s+([A-Z]\w*)"#, esc(&name))).search(&raw) {
                    let e = names.entry(name).or_default();
                    if !e.contains(&nm.s(1).to_string()) {
                        e.push(nm.s(1).to_string());
                    }
                }
                continue;
            }
            let decl = rxd(&format!(r"(?m)(?:^|[;{{}}(\s])([A-Z]\w*)\s*[\*&]?\s*\b{}\b\s*[;({{=]", esc(&var)))
                .search(&raw)
                .map(|d| d.s(1).to_string())
                .or_else(|| {
                    rxd(&format!(r"\b{}\s*=\s*(?:new\s+|std::make_unique<|std::make_shared<)([A-Z]\w*)", esc(&var))).search(&raw).map(|d| d.s(1).to_string())
                });
            if let Some(d) = decl {
                let e = names.entry(name).or_default();
                if !e.contains(&d) {
                    e.push(d);
                }
            }
        }
    }
    for (k, v) in qnames {
        g.qml_names.insert(k, v);
    }
    let mut bound: IndexMap<String, String> = names.iter().filter(|(_, t)| t.len() == 1).map(|(n, t)| (n.clone(), format!("type:{}", t[0]))).collect();
    // an id declared in one QML file only: a child component's `conversationModel`
    // (QML looks ids up through the components that instantiate it) is that one
    let mut ids: IndexMap<String, Vec<String>> = IndexMap::new();
    let add = |ids: &mut IndexMap<String, Vec<String>>, k: &str, v: String| {
        let e = ids.entry(k.to_string()).or_default();
        if !e.contains(&v) {
            e.push(v);
        }
    };
    for rel in &files {
        let mods = &g.by_file[rel];
        if rel.ends_with(".qml") && !mods.is_empty() {
            let m0 = &g.modules[&mods[0]];
            let code = m0.code.join("\n");
            for m in re!(r"(?m)(?:^|[;{])[ \t]*id[ \t]*:[ \t]*([A-Za-z_]\w*)").find_iter(&code) {
                let held = m0.bound.get(m.s(1)).cloned().unwrap_or_default();
                add(&mut ids, m.s(1), if held.starts_with("type:") || held.starts_with("mod:") { held } else { format!("?{rel}") });
            }
            // the root object's own properties: what a child component sees by
            // QML's dynamic scope (`appSettings` of main.qml, in TerminalWindow.qml)
            for m in re!(r"\bproperty[ \t]+[\w.<>]+[ \t]+([a-z_]\w*)").find_iter(&code) {
                if code[..m.start()].matches('{').count() as i64 - code[..m.start()].matches('}').count() as i64 == 1 {
                    let held = m0.bound.get(m.s(1)).cloned().unwrap_or_default();
                    add(&mut ids, m.s(1), if held.starts_with("type:") || held.starts_with("mod:") { held } else { format!("?{rel}") });
                }
            }
        }
    }
    for (n, held) in &ids {
        if held.len() == 1 && (held[0].starts_with("type:") || held[0].starts_with("mod:")) {
            bound.entry(n.clone()).or_insert(held[0].clone());
        }
    }
    if bound.is_empty() {
        return;
    }
    for rel in &files {
        if rel.ends_with(".qml") || rel.ends_with(".js") {
            for mn in g.by_file[rel].clone() {
                let m = g.modules.get_mut(&mn).unwrap();
                for (n, t) in &bound {
                    m.bound.entry(n.clone()).or_insert(t.clone());
                }
            }
        }
    }
}
