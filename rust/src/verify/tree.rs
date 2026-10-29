//! A screen as the driver sees it (for adb: a `uiautomator dump`), and the
//! state line verify records from it. An element's id is its `resource-id`,
//! else its `content-desc` when that reads as an id; text that isn't an id is
//! never taken for one.

use indexmap::IndexMap;
use regex::Regex;
use serde_json::{json, Value};

use crate::glob::fnmatchcase;

#[derive(Clone, Debug, Default)]
pub struct Node {
    pub id: String,
    pub text: String,
    pub package: String,
    pub bounds: [i64; 4],
    pub enabled: bool,
    pub selected: bool,
    pub checked: bool,
    pub parent: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Screen {
    pub nodes: Vec<Node>,
    /// The focused window (package/activity), from the driver.
    pub focus: String,
    pub raw: String,
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(j) = rest.find(';') else { break };
        let ent = &rest[1..j];
        let c = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[j + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parse a uiautomator dump. `id_pattern` decides when a content-desc is an id.
pub fn parse(xml: &str, id_pattern: &Regex) -> Result<Screen, String> {
    let attr = Regex::new(r#"([\w-]+)="([^"]*)""#).unwrap();
    let bounds = Regex::new(r"\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]").unwrap();
    let mut nodes: Vec<Node> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut i = 0;
    let b = xml.as_bytes();
    let mut seen_root = false;
    while let Some(off) = xml[i..].find('<') {
        let start = i + off;
        let Some(end_off) = xml[start..].find('>') else { break };
        let end = start + end_off;
        let tag = &xml[start + 1..end];
        i = end + 1;
        if tag.starts_with("/node") {
            stack.pop();
            continue;
        }
        if tag.starts_with("hierarchy") {
            seen_root = true;
            continue;
        }
        if !tag.starts_with("node") {
            continue;
        }
        let self_closing = b[end - 1] == b'/';
        let mut n = Node { parent: stack.last().copied(), enabled: true, ..Default::default() };
        let mut desc = String::new();
        for c in attr.captures_iter(tag) {
            let v = unescape(&c[2]);
            match &c[1] {
                "resource-id" => n.id = v,
                "content-desc" => desc = v,
                "text" => n.text = v,
                "package" => n.package = v,
                "enabled" => n.enabled = v == "true",
                "selected" => n.selected = v == "true",
                "checked" => n.checked = v == "true",
                "bounds" => {
                    if let Some(m) = bounds.captures(&v) {
                        n.bounds = [m[1].parse().unwrap_or(0), m[2].parse().unwrap_or(0), m[3].parse().unwrap_or(0), m[4].parse().unwrap_or(0)];
                    }
                }
                _ => {}
            }
        }
        // A view id is "<package>:id/<name>": the app's own read as <name>, the framework's are layout, not controls.
        if let Some((pkg, name)) = n.id.split_once(":id/") {
            n.id = if pkg == n.package { name.to_string() } else { String::new() };
        }
        if n.id.is_empty() && id_pattern.is_match(&desc) {
            n.id = desc;
        }
        nodes.push(n);
        if !self_closing {
            stack.push(nodes.len() - 1);
        }
    }
    if !seen_root {
        let head: String = xml.chars().take(200).collect();
        return Err(format!("not a uiautomator dump: {head:?}"));
    }
    Ok(Screen { nodes, focus: String::new(), raw: xml.to_string() })
}

impl Screen {
    pub fn find(&self, id: &str, package: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id && (package.is_empty() || n.package == package))
    }

    pub fn ids(&self, package: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for n in &self.nodes {
            if !n.id.is_empty() && n.package == package && !out.contains(&n.id) {
                out.push(n.id.clone());
            }
        }
        out
    }

    fn ancestors(&self, mut i: usize) -> Vec<usize> {
        let mut out = Vec::new();
        while let Some(p) = self.nodes[i].parent {
            out.push(p);
            i = p;
        }
        out
    }

    /// The text a person reads in a node: its own, else its descendants' in order.
    fn text_of(&self, i: usize, allowed: &dyn Fn(usize) -> bool) -> String {
        if !self.nodes[i].text.is_empty() {
            return self.nodes[i].text.clone();
        }
        let mut parts: Vec<&str> = Vec::new();
        // In document order a node's descendants are the run of nodes right after it.
        for (j, n) in self.nodes.iter().enumerate().skip(i + 1) {
            if !self.ancestors(j).contains(&i) {
                break;
            }
            if !n.text.is_empty() && allowed(j) {
                parts.push(&n.text);
            }
        }
        let t = parts.join(" ");
        t.chars().take(160).collect()
    }

    /// The state line's fields for this screen. With `keep` (a tree capture), only
    /// the kept subtrees' ids and text are recorded, their ancestors' ids without
    /// text, and of everything else only how much there was (`outside`), so a
    /// leak shows as a number and is never written down.
    pub fn record(&self, package: &str, keep: &[String]) -> IndexMap<String, Value> {
        let app: Vec<usize> = (0..self.nodes.len()).filter(|&i| self.nodes[i].package == package).collect();
        let kept: Vec<bool> = if keep.is_empty() {
            vec![true; self.nodes.len()]
        } else {
            let roots: Vec<usize> =
                app.iter().copied().filter(|&i| !self.nodes[i].id.is_empty() && keep.iter().any(|g| fnmatchcase(&self.nodes[i].id, g))).collect();
            (0..self.nodes.len()).map(|i| roots.contains(&i) || self.ancestors(i).iter().any(|a| roots.contains(a))).collect()
        };
        let ancestor_of_kept: Vec<bool> = if keep.is_empty() {
            vec![false; self.nodes.len()]
        } else {
            let mut v = vec![false; self.nodes.len()];
            for i in (0..self.nodes.len()).filter(|&i| kept[i]) {
                for a in self.ancestors(i) {
                    v[a] = true;
                }
            }
            v
        };
        let mut ids: Vec<Value> = Vec::new();
        let mut els = serde_json::Map::new();
        let mut texts: Vec<Value> = Vec::new();
        let (mut out_ids, mut out_texts) = (0, 0);
        let mut out_names: Vec<Value> = Vec::new();
        for &i in &app {
            let n = &self.nodes[i];
            if kept[i] {
                if !n.text.is_empty() {
                    texts.push(json!(n.text));
                }
                if !n.id.is_empty() && !els.contains_key(&n.id) {
                    ids.push(json!(n.id));
                    let text = self.text_of(i, &|j| kept[j]);
                    els.insert(
                        n.id.clone(),
                        json!({"text": text, "enabled": n.enabled, "selected": n.selected, "checked": n.checked}),
                    );
                }
            } else if ancestor_of_kept[i] {
                if !n.id.is_empty() && !ids.contains(&json!(n.id)) {
                    ids.push(json!(n.id));
                }
            } else {
                if !n.id.is_empty() {
                    out_ids += 1;
                    // An id is a control's name, not what it shows; a private area's ids are only counted.
                    if keep.iter().all(|g| !n.id.starts_with(g.split('.').next().unwrap_or(g))) && out_names.len() < 40 {
                        out_names.push(json!(n.id));
                    }
                }
                if !n.text.is_empty() {
                    out_texts += 1;
                }
            }
        }
        let mut m = IndexMap::new();
        m.insert("focus".into(), json!(self.focus));
        m.insert("ids".into(), Value::Array(ids));
        m.insert("els".into(), Value::Object(els));
        m.insert("texts".into(), Value::Array(texts));
        if !keep.is_empty() {
            m.insert("outside".into(), json!({"ids": out_ids, "texts": out_texts, "names": out_names}));
        }
        m
    }

    /// What a step compares to say whether the screen changed: the whole app's nodes, held in memory only
    /// (a tree capture records just its kept part, but a change anywhere is still a change).
    pub fn signature(&self, package: &str) -> String {
        let r = self.record(package, &[]);
        format!("{}|{}|{}|{}", r["focus"], r["ids"], r["els"], r["texts"])
    }

    /// The dump with only what `record` would keep (a tree capture's evidence file).
    pub fn filtered_xml(&self, package: &str, keep: &[String]) -> String {
        let r = self.record(package, keep);
        let mut out = String::from("<?xml version='1.0' encoding='UTF-8' standalone='yes' ?><filtered keep=\"");
        out.push_str(&keep.join(" "));
        out.push_str("\">\n");
        for id in r["ids"].as_array().into_iter().flatten() {
            let id = id.as_str().unwrap_or("");
            let n = self.find(id, package).cloned().unwrap_or_default();
            let el = r["els"].get(id);
            let text = el.and_then(|e| e["text"].as_str()).unwrap_or("");
            out.push_str(&format!(
                "  <node id=\"{}\" text=\"{}\" bounds=\"[{},{}][{},{}]\" />\n",
                xml_escape(id),
                xml_escape(text),
                n.bounds[0],
                n.bounds[1],
                n.bounds[2],
                n.bounds[3]
            ));
        }
        out.push_str(&format!("  <outside value='{}' />\n</filtered>\n", xml_escape(&r.get("outside").map(|o| o.to_string()).unwrap_or_default())));
        out
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// Where on an element a touch lands: its centre, or 12 px inside one edge (`at`).
pub fn point(b: [i64; 4], at: &str) -> Result<(i64, i64), String> {
    let (cx, cy) = ((b[0] + b[2]) / 2, (b[1] + b[3]) / 2);
    let inset = 12;
    let p = match at {
        "" | "center" => (cx, cy),
        "top" => (cx, b[1] + inset),
        "bottom" => (cx, b[3] - inset),
        "left" => (b[0] + inset, cy),
        "right" => (b[2] - inset, cy),
        "top-left" => (b[0] + inset, b[1] + inset),
        "top-right" => (b[2] - inset, b[1] + inset),
        "bottom-left" => (b[0] + inset, b[3] - inset),
        "bottom-right" => (b[2] - inset, b[3] - inset),
        other => return Err(format!("at = '{other}' (center, top, bottom, left, right, or a corner like top-left)")),
    };
    Ok(p)
}
