//! `kind = "design"`: a repo's own design tokens as rules. The engine knows no
//! look; the repo declares it in a `[design]` table and each rule picks a check:
//!
//! * `check = "colour"`: raw colour literals (`#rrggbb`, `rgb()`, `0xAARRGGBB` on a
//!   line that says colour, Kotlin's `Color.Red`) that aren't in the palette.
//! * `check = "scale", scale = "spacing"`: a number given to a property of the
//!   named `[design.scales.<name>]` that isn't one of its values (spacing,
//!   type sizes, corner radii, rule weights).
//! * `check = "font"`: font families outside the declared set, and denied ones.
//!
//! ```toml
//! [design]
//! palette_from = ["design/tokens.json"]     # every "#hex" value in it, role = its key
//! rem = 16
//! allow_literals = ["#000000", "#ffffff", "Color.Transparent"]
//! [design.palette]                          # more roles, inline
//! paper = "#f4f1ea"
//! [design.scales.spacing]
//! props = ["padding", "margin", "gap", "spacing", "spacedBy"]
//! values = "json:design/tokens.json#spacing"  # or [4, 8, 12]
//! units = ["px", "dp"]                      # a number with another unit is not judged
//! also = [0]                                # always fine
//! deny = ['rounded-(sm|md|lg|xl|full)']     # optional: a regex that is itself the finding
//! [design.fonts]
//! allow = ["Chakra Petch", "IBM Plex Sans"]
//! generic = ["sans-serif", "monospace"]
//! deny = ["serif"]                          # words; "sans-serif" doesn't count as serif
//! ```
//!
//! Rules use the usual `in`, `allow`, `severity`, `gate = "changed"`; a line
//! carrying `seamcheck: allow <rule>` is waived. Generated files are skipped.

use std::collections::BTreeMap;

use rayon::prelude::*;
use regex::Regex;
use serde_json::{Map, Value};

use super::{need_str, scope_files, suppressed, Res, RuleErr};
use crate::config::{id, list_or, s, severity, Rule};
use crate::project::{Finding, Mode, Project};

use crate::pyre::{strip, take_chars};

pub fn design(p: &Project, r: &Rule) -> Res<Vec<Finding>> {
    let check = need_str(r, "check")?;
    let d = p.config.table("design");
    let rid = id(r);
    let sev = severity(r);
    let allow = list_or(r, "allow", &[]);
    let judge: Box<dyn Fn(&str, &str) -> Vec<String> + Sync + Send> = match check {
        "colour" | "color" => {
            let pal = Palette::load(p, d);
            Box::new(move |rel, line| pal.judge(rel, line))
        }
        "scale" => {
            let name = need_str(r, "scale")?.to_string();
            let sc = Scale::load(p, d, &name).map_err(RuleErr::BadPattern)?;
            Box::new(move |_, line| sc.judge(&name, line))
        }
        "font" => {
            let f = Fonts::load(d);
            Box::new(move |_, line| f.judge(line))
        }
        other => return Err(RuleErr::BadPattern(format!("design check {other:?} (colour, scale or font)"))),
    };
    let mut files = scope_files(p, r);
    if let Some(ch) = &p.gate_changed {
        files.retain(|f| ch.contains(f));
    }
    let per: Vec<Vec<Finding>> = files
        .par_iter()
        .map(|rel| {
            if super::allowed(p, rel, None, &allow) || crate::gate::is_generated(p, rel) {
                return vec![];
            }
            let raw = p.text_for(rel, Mode::Raw);
            let lines = strip_comments(rel, &raw);
            let mut out = Vec::new();
            for (i, ln) in lines.iter().enumerate() {
                if ln.len() > 2000 || suppressed(&raw, i + 1, rid) {
                    continue;
                }
                for msg in judge(rel, ln) {
                    let func = p.func_at(rel, i + 1);
                    out.push(
                        Finding::new(rid, &sev, rel, i + 1, func.as_ref().map(|f| f.name.as_str()).unwrap_or(""), msg)
                            .snip(take_chars(strip(ln), 160).to_string()),
                    );
                }
            }
            out
        })
        .collect();
    Ok(per.into_iter().flatten().collect())
}

// --- colours -------------------------------------------------------------------------

struct Palette {
    /// rgb -> roles (in file order, first wins for naming)
    by_rgb: BTreeMap<u32, Vec<String>>,
    allowed: Vec<String>,
    strict: bool,
}

fn hex_rx() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| Regex::new(r"#([0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{3})\b").unwrap())
}

fn rgb_fn_rx() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\brgba?\(\s*(\d{1,3})\s*[, ]\s*(\d{1,3})\s*[, ]\s*(\d{1,3})\s*[,/)\s]").unwrap()
    })
}

fn hex_int_rx() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| Regex::new(r"\b0[xX]([0-9a-fA-F]{8}|[0-9a-fA-F]{6})\b").unwrap())
}

fn named_rx() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\bColor\.(Red|Green|Blue|Yellow|Magenta|Cyan|Gray|LightGray|DarkGray|White|Black|Transparent)\b").unwrap()
    })
}

/// "#rgb", "#rrggbb" or "#rrggbbaa" (CSS order) -> rgb
fn parse_css_hex(h: &str) -> Option<u32> {
    let h = h.trim_start_matches('#');
    match h.len() {
        3 => {
            let v = u32::from_str_radix(h, 16).ok()?;
            let (r, g, b) = ((v >> 8) & 15, (v >> 4) & 15, v & 15);
            Some((r * 17) << 16 | (g * 17) << 8 | b * 17)
        }
        6 => u32::from_str_radix(h, 16).ok(),
        8 => Some(u32::from_str_radix(h, 16).ok()? >> 8),
        _ => None,
    }
}

fn collect_hex(v: &Value, key: &str, out: &mut Vec<(String, u32)>) {
    match v {
        Value::String(t) if t.starts_with('#') && matches!(t.len(), 7 | 9) => {
            if let Some(rgb) = parse_css_hex(t) {
                out.push((key.to_string(), rgb));
            }
        }
        Value::Object(o) => o.iter().for_each(|(k, x)| collect_hex(x, k, out)),
        Value::Array(a) => a.iter().for_each(|x| collect_hex(x, key, out)),
        _ => {}
    }
}

impl Palette {
    fn load(p: &Project, d: &Map<String, Value>) -> Palette {
        let mut found: Vec<(String, u32)> = Vec::new();
        for f in list_or(d, "palette_from", &[]) {
            if let Some(t) = crate::project::read_text(&p.path(&f)) {
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    collect_hex(&v, "", &mut found);
                }
            }
        }
        if let Some(Value::Object(o)) = d.get("palette") {
            o.iter().for_each(|(k, x)| collect_hex(x, k, &mut found));
        }
        let mut by_rgb: BTreeMap<u32, Vec<String>> = BTreeMap::new();
        for (role, rgb) in found {
            let roles = by_rgb.entry(rgb).or_default();
            if !roles.contains(&role) {
                roles.push(role);
            }
        }
        let allowed = list_or(d, "allow_literals", &[]).into_iter().map(|a| a.to_lowercase()).collect();
        Palette { by_rgb, allowed, strict: crate::config::truthy(d.get("strict")) }
    }

    fn judge(&self, rel: &str, line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let css = rel.ends_with(".css") || rel.ends_with(".scss");
        for c in hex_rx().captures_iter(line) {
            let m = c.get(0).unwrap();
            // `#{x}` and id selectors aren't colours; a 3-digit hex is one only in a stylesheet
            if c[1].len() == 3 && !css {
                continue;
            }
            if line[..m.start()].ends_with(|ch: char| ch.is_alphanumeric() || ch == '&' || ch == '/') {
                continue;
            }
            // QML writes eight digits as #AARRGGBB, CSS as #RRGGBBAA
            let rgb = if c[1].len() == 8 && rel.ends_with(".qml") {
                u32::from_str_radix(&c[1], 16).ok().map(|v| v & 0x00ff_ffff)
            } else {
                parse_css_hex(m.as_str())
            };
            if let Some(rgb) = rgb {
                self.one(m.as_str(), rgb, &mut out);
            }
        }
        for c in rgb_fn_rx().captures_iter(line) {
            let n = |i: usize| c[i].parse::<u32>().unwrap_or(256);
            if n(1) < 256 && n(2) < 256 && n(3) < 256 {
                self.one(c.get(0).unwrap().as_str().trim_end_matches(|ch: char| !ch.is_ascii_digit()), n(1) << 16 | n(2) << 8 | n(3), &mut out);
            }
        }
        if line.to_lowercase().contains("color") {
            for c in hex_int_rx().captures_iter(line) {
                let v = u32::from_str_radix(&c[1], 16).unwrap_or(0);
                let rgb = if c[1].len() == 8 { v & 0x00ff_ffff } else { v };
                self.one(c.get(0).unwrap().as_str(), rgb, &mut out);
            }
        }
        for c in named_rx().find_iter(line) {
            if !self.allowed.contains(&c.as_str().to_lowercase()) {
                out.push(format!("raw colour {} is not a role of the palette; use the theme's colour role", c.as_str()));
            }
        }
        out
    }

    fn one(&self, lit: &str, rgb: u32, out: &mut Vec<String>) {
        if self.allowed.contains(&lit.to_lowercase()) || self.allowed.contains(&format!("#{:06x}", rgb)) {
            return;
        }
        match self.by_rgb.get(&rgb) {
            Some(roles) if self.strict => out.push(format!("raw colour {lit} is the role {}; use the role, not the literal", roles[0])),
            Some(_) => {}
            None => {
                let near = self
                    .by_rgb
                    .iter()
                    .min_by_key(|(c, _)| dist(**c, rgb))
                    .map(|(c, roles)| format!(" (nearest: {} #{:06x})", roles[0], c))
                    .unwrap_or_default();
                out.push(format!("raw colour {lit} is not in the palette{near}; use a theme colour role"));
            }
        }
    }
}

fn dist(a: u32, b: u32) -> u32 {
    let c = |x: u32, s: u32| ((x >> s) & 255) as i32;
    [16, 8, 0].iter().map(|&s| (c(a, s) - c(b, s)).pow(2) as u32).sum()
}

// --- scales --------------------------------------------------------------------------

struct Scale {
    props: Vec<Vec<String>>,
    /// words that rule an identifier out (`letter` for letter-spacing)
    not: Vec<String>,
    values: Vec<f64>,
    units: Vec<String>,
    rem: f64,
    deny: Option<Regex>,
}

/// "json:file#a.b.*": the numbers at a path (`*` for every key).
fn json_numbers(p: &Project, spec: &str) -> Result<Vec<f64>, String> {
    let (file, path) = spec.trim_start_matches("json:").split_once('#').unwrap_or((spec, ""));
    let t = crate::project::read_text(&p.path(file)).ok_or_else(|| format!("{file}: can't read"))?;
    let v: Value = serde_json::from_str(&t).map_err(|e| format!("{file}: {e}"))?;
    let mut cur = vec![&v];
    for seg in path.split('.').filter(|s| !s.is_empty()) {
        cur = cur
            .into_iter()
            .flat_map(|x| match (x, seg) {
                (Value::Object(o), "*") => o.values().collect::<Vec<_>>(),
                (Value::Object(o), k) => o.get(k).into_iter().collect(),
                _ => vec![],
            })
            .collect();
    }
    let mut out = Vec::new();
    for x in cur {
        match x {
            Value::Number(n) => out.extend(n.as_f64()),
            Value::Array(a) => out.extend(a.iter().filter_map(|y| y.as_f64())),
            _ => {}
        }
    }
    Ok(out)
}

fn numbers(p: &Project, v: Option<&Value>) -> Result<Vec<f64>, String> {
    match v {
        Some(Value::String(t)) => json_numbers(p, t),
        Some(Value::Array(a)) => Ok(a.iter().filter_map(|x| x.as_f64()).collect()),
        _ => Ok(vec![]),
    }
}

impl Scale {
    fn load(p: &Project, d: &Map<String, Value>, name: &str) -> Result<Scale, String> {
        let sc = d
            .get("scales")
            .and_then(|s| s.get(name))
            .and_then(|s| s.as_object())
            .ok_or_else(|| format!("[design.scales.{name}] isn't declared"))?;
        let mut values = numbers(p, sc.get("values"))?;
        values.extend(match sc.get("also") {
            None => vec![0.0],
            some => numbers(p, some)?,
        });
        let deny = match sc.get("deny").and_then(|x| x.as_str()) {
            Some(t) => Some(Regex::new(t).map_err(|e| e.to_string())?),
            None => None,
        };
        Ok(Scale {
            props: list_or(sc, "props", &[]).into_iter().map(|x| norm_parts(&x)).collect(),
            values,
            not: list_or(sc, "not", &[]).into_iter().map(|x| x.to_lowercase()).collect(),
            units: list_or(sc, "units", &["px"]),
            rem: d.get("rem").and_then(|x| x.as_f64()).unwrap_or(16.0),
            deny,
        })
    }

    fn is_prop(&self, ident: &str) -> bool {
        let parts = norm_parts(ident);
        if parts.iter().any(|w| self.not.contains(w)) {
            return false;
        }
        self.props.iter().any(|p| {
            !p.is_empty()
                && parts.windows(p.len()).any(|w| {
                    let n = p.len() - 1;
                    w[..n] == p[..n] && (w[n] == p[n] || w[n] == format!("{}s", p[n]))
                })
        })
    }

    fn judge(&self, name: &str, line: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(rx) = &self.deny {
            for m in rx.find_iter(line) {
                out.push(format!("{} is outside the {name} scale", m.as_str()));
            }
        }
        for c in ident_rx().captures_iter(line) {
            let (ident, sep) = (&c[1], &c[2]);
            if !self.is_prop(ident) {
                continue;
            }
            let at = c.get(0).unwrap().end();
            let mut bad = |n: f64, shown: &str| {
                if n > 0.0 || n < 0.0 {
                    let px = n;
                    if !self.values.iter().any(|v| (v - px).abs() < 0.01) {
                        out.push(format!("{ident} {shown} is not on the {name} scale ({})", fmt_scale(&self.values)));
                    }
                }
            };
            if sep.trim_end().ends_with('(') {
                let end = crate::lint::balanced_end(line, at, "");
                for m in num_rx().captures_iter(&line[at..end]) {
                    if m.get(2).is_some() {
                        self.number(&m, &mut bad);
                    }
                }
            } else {
                // `prop: 8 12px` — a run of values; stops at the first thing that isn't one
                let mut rest = &line[at..];
                while let Some(m) = num_start_rx().captures(rest) {
                    let whole = m.get(0).unwrap();
                    let c = num_rx().captures(whole.as_str()).unwrap();
                    self.number(&c, &mut bad);
                    rest = &rest[whole.end()..];
                }
            }
        }
        out
    }

    fn number(&self, c: &regex::Captures, bad: &mut impl FnMut(f64, &str)) {
        let Ok(n) = c[1].parse::<f64>() else { return };
        let unit = c.get(2).map(|u| u.as_str()).unwrap_or("");
        if !unit.is_empty() && !self.units.iter().any(|u| u == unit) {
            return;
        }
        let px = if unit == "rem" { n * self.rem } else { n };
        bad(px, c.get(0).unwrap().as_str());
    }
}

fn fmt_scale(v: &[f64]) -> String {
    let mut f = v.to_vec();
    f.sort_by(|a, b| b.partial_cmp(a).unwrap());
    f.dedup();
    let v: Vec<String> = f.iter().map(|x| format!("{x}")).collect();
    v.join(" ")
}

fn ident_rx() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| Regex::new(r"([A-Za-z_][\w.\-]*)(\s*[:=]\s*\{?\s*|\s*\()").unwrap())
}

/// a number, with an optional unit: `16`, `12px`, `0.5rem`, `16.dp`
fn num_rx() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| Regex::new(r"(-?\d+(?:\.\d+)?)(?:\.?([a-z%]+))?").unwrap())
}

fn num_start_rx() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\s*-?\d+(?:\.\d+)?(?:\.?[a-z%]+)?(?:\s|;|,|\}|\)|$)").unwrap())
}

/// `paddingTop` / `padding_top` / `row-gap` / `anchors.margins` -> lowercase words
fn norm_parts(ident: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for ch in ident.chars() {
        if matches!(ch, '_' | '-' | '.') {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if ch.is_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = ch.is_lowercase() || ch.is_ascii_digit();
        cur.extend(ch.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

// --- fonts ---------------------------------------------------------------------------

struct Fonts {
    allow: Vec<String>,
    generic: Vec<String>,
    deny: Vec<String>,
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase()
}

fn family_rx() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?i)\bfont[-_ ]?family\s*[:=]\s*([^;}\n]*)|\bR\.font\.(\w+)|\bFontFamily\.(\w+)"#).unwrap())
}

impl Fonts {
    fn load(d: &Map<String, Value>) -> Fonts {
        let f = d.get("fonts").and_then(|x| x.as_object()).cloned().unwrap_or_default();
        let sq = |k: &str, def: &[&str]| list_or(&f, k, def).iter().map(|x| squash(x)).filter(|x| !x.is_empty()).collect();
        Fonts {
            allow: sq("allow", &[]),
            generic: sq("generic", &["sans-serif", "monospace", "system-ui", "ui-monospace", "ui-sans-serif", "inherit", "initial", "unset"]),
            deny: sq("deny", &[]),
        }
    }

    fn judge(&self, line: &str) -> Vec<String> {
        let mut out = Vec::new();
        for c in family_rx().captures_iter(line) {
            let names: Vec<String> = match (c.get(1), c.get(2), c.get(3)) {
                (Some(l), _, _) => l.as_str().split(',').map(|x| x.trim().trim_matches(|ch| ch == '"' || ch == '\'' || ch == '`').trim().to_string()).collect(),
                (_, Some(n), _) | (_, _, Some(n)) => vec![n.as_str().to_string()],
                _ => vec![],
            };
            for name in names {
                if name.is_empty() || name.contains("var(") || name.contains('$') || name.contains("--") || name.starts_with('{') || name.starts_with('@') {
                    continue;
                }
                // a reference to a font defined elsewhere (`Face.display`, `VeridianFonts.Mono`), not a family name
                if !name.contains(' ') && name.trim_end_matches(')').contains('.') {
                    continue;
                }
                let sq = squash(&name);
                if sq.is_empty() || self.generic.contains(&sq) && !self.deny.contains(&sq) {
                    continue;
                }
                let no_sans = sq.replace("sansserif", "");
                if let Some(w) = self.deny.iter().find(|w| no_sans.contains(w.as_str())) {
                    out.push(format!("font {name:?} is denied ({w})"));
                } else if !self.allow.is_empty() && !self.allow.iter().any(|a| sq.contains(a.as_str())) {
                    out.push(format!("font {name:?} is not one of the family's fonts"));
                }
            }
        }
        out
    }
}

// --- comments ------------------------------------------------------------------------

/// The lines with comments blanked and strings kept (the engine's own modes
/// treat `#` as a comment in a stylesheet, which a hex colour is not).
fn strip_comments(rel: &str, raw: &[String]) -> Vec<String> {
    let hash = matches!(
        rel.rsplit('.').next().unwrap_or(""),
        "ex" | "exs" | "py" | "rb" | "sh" | "fish" | "toml" | "yml" | "yaml"
    );
    let elixir = rel.ends_with(".ex") || rel.ends_with(".exs");
    let mut block = false;
    let mut heredoc = false;
    let mut out = Vec::with_capacity(raw.len());
    for ln in raw {
        let b = ln.as_bytes();
        if elixir && ln.contains("\"\"\"") && ln.matches("\"\"\"").count() % 2 == 1 {
            let opens = !heredoc;
            heredoc = opens && (ln.contains("@moduledoc") || ln.contains("@doc") || ln.contains("@typedoc"));
            out.push(String::new());
            continue;
        }
        if heredoc {
            out.push(String::new());
            continue;
        }
        let mut s = String::with_capacity(ln.len());
        let mut i = 0;
        let mut quote: Option<u8> = None;
        while i < b.len() {
            let c = b[i];
            if block {
                if c == b'*' && b.get(i + 1) == Some(&b'/') {
                    block = false;
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if let Some(q) = quote {
                if c == b'\\' {
                    s.push_str(&ln[i..(i + 2).min(ln.len())]);
                    i += 2;
                    continue;
                }
                if c == q {
                    quote = None;
                }
            } else if c == b'"' || c == b'`' || (c == b'\'' && closes_later(b, i)) {
                quote = Some(c);
            } else if c == b'/' && b.get(i + 1) == Some(&b'*') && !hash {
                block = true;
                i += 2;
                continue;
            } else if (c == b'/' && b.get(i + 1) == Some(&b'/') && !hash && (i == 0 || b[i - 1] != b':')) || (hash && c == b'#') {
                break;
            }
            // (a char boundary: copy the whole character)
            let n = ln[i..].chars().next().map(|ch| ch.len_utf8()).unwrap_or(1);
            s.push_str(&ln[i..i + n]);
            i += n;
        }
        out.push(s);
    }
    out
}

/// A single quote opens a string only when another closes it on the line
/// (Rust lifetimes and apostrophes in prose don't).
fn closes_later(b: &[u8], i: usize) -> bool {
    b[i + 1..].contains(&b'\'')
}
