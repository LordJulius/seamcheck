//! Output in Python's exact shapes: `json.dumps(obj, indent=1)` (ASCII-escaped),
//! the flat finding dicts, SARIF and GitHub annotations (output.py).

use serde_json::{json, Map, Value};

use crate::project::Finding;

fn esc_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", u));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn dump(v: &Value, indent: usize, level: usize, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => esc_str(s, out),
        Value::Array(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&" ".repeat(indent * (level + 1)));
                dump(x, indent, level + 1, out);
            }
            out.push('\n');
            out.push_str(&" ".repeat(indent * level));
            out.push(']');
        }
        Value::Object(o) => {
            if o.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, x)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&" ".repeat(indent * (level + 1)));
                esc_str(k, out);
                out.push_str(": ");
                dump(x, indent, level + 1, out);
            }
            out.push('\n');
            out.push_str(&" ".repeat(indent * level));
            out.push('}');
        }
    }
}

/// `json.dumps(v, indent=1)`
pub fn dumps(v: &Value) -> String {
    let mut s = String::new();
    dump(v, 1, 0, &mut s);
    s
}

/// `json.dumps(v)` (one line: `, ` and `: ` separators)
pub fn dumps_line(v: &Value) -> String {
    match v {
        Value::Array(a) => format!("[{}]", a.iter().map(dumps_line).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, x)| {
                    let mut s = String::new();
                    esc_str(k, &mut s);
                    format!("{s}: {}", dumps_line(x))
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => {
            let mut s = String::new();
            dump(other, 0, 0, &mut s);
            s
        }
    }
}

/// queries.flat: a finding as `check --json` prints it.
pub fn flat(f: &Finding, why: &str, fix: &str) -> Value {
    json!({"rule": f.rule, "severity": f.severity, "file": f.file, "line": f.line, "func": f.func,
           "message": f.message, "snippet": f.snippet, "why": why, "fix": fix})
}

fn esc_data(s: &str) -> String {
    s.replace('%', "%25").replace('\r', "%0D").replace('\n', "%0A")
}

fn esc_prop(s: &str) -> String {
    esc_data(s).replace(':', "%3A").replace(',', "%2C")
}

fn get<'a>(f: &'a Value, k: &str) -> &'a str {
    f.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

pub fn github(findings: &[Value]) -> String {
    let mut out = Vec::new();
    for f in findings {
        let kind = match get(f, "severity") {
            "warn" => "warning",
            "info" => "notice",
            _ => "error",
        };
        let mut props = Vec::new();
        if !get(f, "file").is_empty() {
            props.push(format!("file={}", esc_prop(get(f, "file"))));
            let line = f.get("line").and_then(|x| x.as_u64()).unwrap_or(0);
            if line > 0 {
                props.push(format!("line={line}"));
            }
        }
        props.push(format!("title={}", esc_prop(&format!("seamcheck {}", get(f, "rule")))));
        let mut msg = get(f, "message").to_string();
        if !get(f, "why").is_empty() {
            msg.push_str(&format!(" - {}", get(f, "why")));
        }
        if !get(f, "fix").is_empty() {
            msg.push_str(&format!(" (fix: {})", get(f, "fix")));
        }
        out.push(format!("::{kind} {}::{}", props.join(","), esc_data(&msg)));
    }
    out.join("\n")
}

pub fn sarif(findings: &[Value], rules: &[(String, String, String)]) -> String {
    let mut meta: std::collections::BTreeMap<String, (String, String)> =
        rules.iter().map(|(id, w, f)| (id.clone(), (w.clone(), f.clone()))).collect();
    for f in findings {
        meta.entry(get(f, "rule").to_string()).or_insert((get(f, "why").to_string(), get(f, "fix").to_string()));
    }
    let ids: Vec<&String> = meta.keys().collect();
    let descriptors: Vec<Value> = meta
        .iter()
        .map(|(id, (why, fix))| {
            let mut d = Map::new();
            d.insert("id".into(), json!(id));
            d.insert("name".into(), json!(id));
            if !why.is_empty() {
                d.insert("shortDescription".into(), json!({"text": why}));
            }
            if !fix.is_empty() {
                d.insert("help".into(), json!({"text": fix}));
            }
            Value::Object(d)
        })
        .collect();
    let results: Vec<Value> = findings
        .iter()
        .map(|f| {
            let rid = get(f, "rule");
            let level = match get(f, "severity") {
                "warn" => "warning",
                "info" => "note",
                _ => "error",
            };
            let mut text = get(f, "message").to_string();
            if !get(f, "fix").is_empty() {
                text.push_str(&format!("  (fix: {})", get(f, "fix")));
            }
            let mut r = Map::new();
            r.insert("ruleId".into(), json!(rid));
            r.insert("ruleIndex".into(), json!(ids.iter().position(|x| *x == rid).unwrap_or(0)));
            r.insert("level".into(), json!(level));
            r.insert("message".into(), json!({"text": text}));
            if !get(f, "file").is_empty() {
                let mut loc = Map::new();
                loc.insert("artifactLocation".into(), json!({"uri": get(f, "file")}));
                let line = f.get("line").and_then(|x| x.as_u64()).unwrap_or(0);
                if line > 0 {
                    loc.insert("region".into(), json!({"startLine": line}));
                }
                r.insert("locations".into(), json!([{"physicalLocation": Value::Object(loc)}]));
            }
            Value::Object(r)
        })
        .collect();
    let log = json!({"$schema": "https://json.schemastore.org/sarif-2.1.0.json", "version": "2.1.0",
        "runs": [{"tool": {"driver": {"name": "seamcheck", "version": crate::VERSION,
                                      "informationUri": "https://github.com/LordJulius/seamcheck",
                                      "rules": descriptors}},
                  "results": results}]});
    dumps(&log)
}
