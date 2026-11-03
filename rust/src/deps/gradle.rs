//! build.gradle[.kts] (deps._gradle): declarations (literal or through
//! libs.versions.toml), the packages each artifact's Gradle-cache jar / aar
//! holds, what its POM brings along, local jars' class references, and
//! package names in Kotlin / Java / XML / ProGuard files.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Seek};
use std::sync::OnceLock;

use indexmap::IndexMap;
use serde_json::Value;

use super::fsx::{self, dirname, glob, home, join, line_of, Liner};
use super::{find_after, lit, py_word, Ctx, Dep};
use crate::pyre::Rx;

const CONF: &[(&str, &str)] = &[
    ("implementation", "runtime"),
    ("api", "runtime"),
    ("compileOnly", "runtime"),
    ("runtimeOnly", "runtime"),
    ("testImplementation", "test"),
    ("androidTestImplementation", "test"),
    ("testRuntimeOnly", "test"),
    ("debugImplementation", "dev"),
    ("kapt", "tool"),
    ("ksp", "tool"),
    ("annotationProcessor", "tool"),
    ("coreLibraryDesugaring", "tool"),
    ("lintChecks", "tool"),
];

fn toml_json(text: &str) -> Option<Value> {
    text.parse::<toml::Table>().ok().map(|t| crate::config::toml_to_json(toml::Value::Table(t)))
}

/// libs.versions.toml accessor ("libs.androidx.core.ktx") -> "group:artifact" (`_catalog`).
fn catalog(ctx: &Ctx, start: &str) -> HashMap<String, String> {
    static SEP: OnceLock<Rx> = OnceLock::new();
    let mut d = start.to_string();
    let path = loop {
        let p = ctx.root.join(join(&d, "gradle/libs.versions.toml"));
        if p.exists() {
            break p;
        }
        if d.is_empty() {
            return HashMap::new();
        }
        d = dirname(&d).to_string();
    };
    let Some(cat) = std::fs::read(&path).ok().and_then(|b| String::from_utf8(b).ok()).and_then(|t| toml_json(&t))
    else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    let Some(libs) = cat.get("libraries").and_then(|l| l.as_object()) else { return out };
    for (alias, v) in libs {
        let ga = match v {
            Value::String(s) => s.split(':').take(2).collect::<Vec<_>>().join(":"),
            Value::Object(o) if o.contains_key("module") => crate::config::py_str(&o["module"]),
            Value::Object(o) => format!(
                "{}:{}",
                o.get("group").map(crate::config::py_str).unwrap_or_else(|| "None".into()),
                o.get("name").map(crate::config::py_str).unwrap_or_else(|| "None".into())
            ),
            _ => continue,
        };
        out.insert(format!("libs.{}", lit(&SEP, r"[-_]").sub(".", alias)), ga);
    }
    out
}

fn cache_base(group: &str) -> String {
    join(&join(&home(), ".gradle/caches/modules-2/files-2.1"), group)
}

type Zip<R> = zip::ZipArchive<R>;

fn open_file(path: &str) -> Option<Zip<std::fs::File>> {
    zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()
}

fn open_bytes(bytes: Vec<u8>) -> Option<Zip<Cursor<Vec<u8>>>> {
    zip::ZipArchive::new(Cursor::new(bytes)).ok()
}

fn names<R: Read + Seek>(z: &Zip<R>) -> Vec<String> {
    z.file_names().map(String::from).collect()
}

fn read_entry<R: Read + Seek>(z: &mut Zip<R>, name: &str) -> Option<Vec<u8>> {
    let mut f = z.by_name(name).ok()?;
    let mut out = Vec::new();
    f.read_to_end(&mut out).ok()?;
    Some(out)
}

/// Packages a Gradle-cache jar / aar holds (`_jar_packages`); `plugin`
/// collects why it runs without being imported: META-INF/services, or
/// components in the library's AndroidManifest.
fn jar_packages(group: &str, artifact: &str, plugin: &mut Vec<String>) -> HashSet<String> {
    static COMP: OnceLock<Rx> = OnceLock::new();
    let base = cache_base(group);
    let mut pk: HashSet<String> = HashSet::new();
    for art in
        [artifact.to_string(), format!("{artifact}-android"), format!("{artifact}-jvm"), format!("{artifact}-desktop")]
    {
        let dir = join(&join(&join(&base, &art), "*"), "*");
        let mut files = glob(&join(&dir, &format!("{art}-*.jar")));
        files.extend(glob(&join(&dir, &format!("{art}-*.aar"))));
        for f in files {
            if f.ends_with("-sources.jar") || f.ends_with("-javadoc.jar") {
                continue;
            }
            let Some(mut z) = open_file(&f) else { continue };
            let mut ns = names(&z);
            if ns.iter().any(|n| n == "AndroidManifest.xml") {
                let Some(man) = read_entry(&mut z, "AndroidManifest.xml") else { continue };
                let man = String::from_utf8_lossy(&man);
                let comp: std::collections::BTreeSet<&str> =
                    lit(&COMP, r"<(provider|service|receiver|activity)\b").findall(&man).into_iter().collect();
                if !comp.is_empty() {
                    plugin.push(format!("its manifest declares {}", comp.into_iter().collect::<Vec<_>>().join(", ")));
                }
            }
            if f.ends_with(".aar") && ns.iter().any(|n| n == "classes.jar") {
                let Some(inner) = read_entry(&mut z, "classes.jar").and_then(open_bytes) else { continue };
                ns = names(&inner);
            }
            let svc: Vec<&str> = ns
                .iter()
                .filter(|n| n.starts_with("META-INF/services/") && !n.ends_with('/'))
                .map(|n| n.rsplit('/').next().unwrap_or(n))
                .collect();
            if !svc.is_empty() {
                plugin.push(format!("provides {}", svc[..svc.len().min(2)].join(", ")));
            }
            for n in &ns {
                if n.ends_with(".class") && n.contains('/') && !n.starts_with("META-INF") {
                    pk.insert(n[..n.rfind('/').unwrap()].replace('/', "."));
                }
            }
            if !pk.is_empty() {
                return pk;
            }
        }
    }
    pk
}

fn word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `re.findall(rb"(?<![\w/])([a-z]\w*(?:/[\w$]+){2,})", data)`.
fn class_names(data: &[u8], out: &mut HashSet<String>) {
    let n = data.len();
    let mut i = 0;
    while i < n {
        if !data[i].is_ascii_lowercase() || (i > 0 && (word(data[i - 1]) || data[i - 1] == b'/')) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < n && word(data[j]) {
            j += 1;
        }
        let mut segs = 0;
        while j < n && data[j] == b'/' && j + 1 < n && (word(data[j + 1]) || data[j + 1] == b'$') {
            j += 1;
            while j < n && (word(data[j]) || data[j] == b'$') {
                j += 1;
            }
            segs += 1;
        }
        if segs >= 2 {
            out.insert(String::from_utf8_lossy(&data[i..j]).into_owned());
            i = j;
        } else {
            i += 1;
        }
    }
}

fn refs_in<R: Read + Seek>(mut z: Zip<R>, out: &mut HashSet<String>) {
    for n in names(&z) {
        if n.ends_with(".class") {
            let Some(data) = read_entry(&mut z, &n) else { return };
            class_names(&data, out);
        }
    }
}

/// Class names (a/b/C) the classes in a jar / aar refer to (`_class_refs`).
fn class_refs(path: &std::path::Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let Some(mut z) = open_file(&path.to_string_lossy()) else { return out };
    if path.to_string_lossy().ends_with(".aar") {
        if !names(&z).iter().any(|n| n == "classes.jar") {
            return out;
        }
        if let Some(inner) = read_entry(&mut z, "classes.jar").and_then(open_bytes) {
            refs_in(inner, &mut out);
        }
    } else {
        refs_in(z, &mut out);
    }
    out
}

/// The deepest package of `parts[..k]` (k from `top` down to 1) the index knows.
fn hit<'a>(idx: &'a HashMap<String, HashSet<String>>, parts: &[&str], top: usize) -> Option<&'a HashSet<String>> {
    (1..top.max(1)).rev().find_map(|k| idx.get(&parts[..k].join(".")))
}

pub fn read(ctx: &Ctx, mrel: &str) -> Vec<Dep> {
    static DEP: OnceLock<Rx> = OnceLock::new();
    static DEPBLOCK: OnceLock<Rx> = OnceLock::new();
    static GROUP: OnceLock<Rx> = OnceLock::new();
    static ARTIFACT: OnceLock<Rx> = OnceLock::new();
    static SCOPE: OnceLock<Rx> = OnceLock::new();
    static FILES: OnceLock<Rx> = OnceLock::new();
    static JVM: OnceLock<Rx> = OnceLock::new();
    let mdir = dirname(mrel);
    let text = ctx.read(mrel);
    let cat = catalog(ctx, mdir);
    let confs: Vec<&str> = CONF.iter().map(|c| c.0).collect();
    let dep_rx = DEP.get_or_init(|| {
        Rx::lit(&format!(
            r#"\b({})\s*\(?\s*(platform\s*\(\s*)?(libs\.[\w.]+|["']([^"':]+:[^"':]+)(?::[^"']*)?["'])"#,
            confs.join("|")
        ))
    });
    let mut deps: IndexMap<String, Dep> = IndexMap::new();
    let blanked = crate::scan::blank_text(&text, "kotlin", true);
    for m in dep_rx.find_iter(&blanked) {
        let ga = match m.get(4) {
            Some(l) if !l.is_empty() => l.to_string(),
            _ => match cat.get(m.s(3).trim_end_matches('.')) {
                Some(g) if !g.is_empty() => g.clone(),
                _ => continue,
            },
        };
        let scope = if m.get(2).is_some_and(|p| !p.is_empty()) {
            "bom"
        } else {
            CONF.iter().find(|c| c.0 == m.s(1)).map(|c| c.1).unwrap_or("runtime")
        };
        if deps.get(&ga).is_none_or(|d| d.scope != "runtime") {
            deps.insert(ga.clone(), Dep::new("gradle", &ga, mrel, line_of(&text, m.start()), scope));
        }
    }
    let mut pkg_index: HashMap<String, HashSet<String>> = HashMap::new();
    for (ga, dep) in deps.iter_mut() {
        if dep.scope == "bom" {
            continue;
        }
        let (group, art) = ga.split_once(':').unwrap_or((ga, ""));
        let mut why: Vec<String> = Vec::new();
        let mut pks = jar_packages(group, art, &mut why);
        let mut uniq: Vec<String> = Vec::new();
        for w in why {
            if !uniq.contains(&w) {
                uniq.push(w);
            }
        }
        dep.plugin = uniq.join("; ");
        if pks.is_empty() {
            dep.guessed = true;
            pks = [group.to_string()].into();
        }
        for p in pks {
            pkg_index.entry(p).or_default().insert(ga.clone());
        }
    }
    // what each declared artifact's POM brings along (optional ones must be declared)
    let all: Vec<String> = deps.keys().cloned().collect();
    for ga in &all {
        let (group, art) = ga.split_once(':').unwrap_or((ga, ""));
        let base = cache_base(group);
        for a in [art.to_string(), format!("{art}-android"), format!("{art}-jvm")] {
            let mut poms = glob(&join(&join(&join(&join(&base, &a), "*"), "*"), "*.pom"));
            if poms.is_empty() {
                continue;
            }
            poms.sort();
            let pom = fsx::read(std::path::Path::new(poms.last().unwrap()));
            for d in lit(&DEPBLOCK, r"(?s)<dependency>(.*?)</dependency>").findall(&pom) {
                let (Some(g), Some(a)) = (
                    lit(&GROUP, r"<groupId>\s*([^<\s]+)").search(d),
                    lit(&ARTIFACT, r"<artifactId>\s*([^<\s]+)").search(d),
                ) else {
                    continue;
                };
                let need = format!("{}:{}", g.s(1), a.s(1));
                if need != *ga && !lit(&SCOPE, r"<scope>\s*(test|provided)").is_match(d) {
                    if let Some(nd) = deps.get_mut(&need) {
                        (if d.contains("<optional>true") { &mut nd.needed_by } else { &mut nd.via }).push(ga.clone());
                    }
                }
            }
            break;
        }
    }
    // local jars / aars (`files("libs/x.aar")`) are compiled code: their class
    // files name the packages they need (Spotify auth -> androidx.browser)
    for fm in lit(&FILES, r#"files\s*\(\s*["']([^"']+\.(?:aar|jar))["']"#).find_iter(&text) {
        let lib = fsx::normpath(&join(if mdir.is_empty() { "." } else { mdir }, fm.s(1)));
        for r in class_refs(&ctx.root.join(&lib)) {
            let parts: Vec<&str> = r.split('/').collect();
            if let Some(h) = hit(&pkg_index, &parts, parts.len()) {
                for ga in h {
                    let code = &mut deps[ga].code;
                    if !code.iter().any(|(f, l)| *f == lib && *l == 0) {
                        code.push((lib.clone(), 0));
                    }
                }
            }
        }
    }
    let jvm = lit(&JVM, r"([a-z][\w]*(?:\.[\w]+){1,})"); // (?<![\w.])
    for rel in ctx.walk(mdir, &[".kt", ".java", ".kts", ".xml", ".pro"], &["build.gradle.kts", "build.gradle"]) {
        if rel == mrel {
            continue;
        }
        let raw = ctx.read(&rel);
        let code = if rel.ends_with(".xml") || rel.ends_with(".pro") { raw } else { ctx.code(&rel, &raw, "kotlin") };
        let lof = Liner::new(&code);
        let mut seen: HashSet<String> = HashSet::new();
        for r in find_after(jvm, &code, |c| py_word(c) || c == '.', |_| true, None) {
            let parts: Vec<&str> = r.s(1).split('.').collect();
            if let Some(h) = hit(&pkg_index, &parts, parts.len()) {
                for ga in h {
                    if seen.insert(ga.clone()) {
                        deps[ga].add(&rel, lof.line(r.start()), None);
                    }
                }
            }
        }
    }
    deps.into_values().filter(|d| d.scope != "bom").collect()
}
