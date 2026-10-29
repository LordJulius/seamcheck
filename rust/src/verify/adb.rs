//! The Android driver: `adb` against one named device. It reads the screen
//! with `uiautomator dump`, touches a control at its bounds from a fresh dump
//! (a real touch, so whatever is on top gets it), and never acts while the
//! app is not in front, the device is locked, or the device is denied.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{json, Value};

use super::config::{Step, Target};
use super::driver::{Check, Driver};
use super::tree::{self, Screen};
use crate::glob::fnmatchcase;

pub struct Adb {
    serial: String,
    package: String,
    activity: String,
    reset_uri: String,
    ready: String,
    ready_timeout: f64,
    start: bool,
    release_only: bool,
    logcat: Vec<String>,
    id_pattern: Regex,
    log_since: String,
    deny: Vec<String>,
}

/// `$NAME` / `${NAME}` from the environment.
pub fn expand(s: &str) -> String {
    let re = Regex::new(r"\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?").unwrap();
    re.replace_all(s, |c: &regex::Captures| std::env::var(&c[1]).unwrap_or_default()).into_owned()
}

impl Adb {
    pub fn new(t: &Target) -> Result<Adb, String> {
        let serial = expand(&t.str("adb", "device"));
        if serial.is_empty() {
            return Err(format!(
                "[adb] device is empty (it is {:?}): verify never picks a device, name one",
                t.str("adb", "device")
            ));
        }
        let pat = t.str("adb", "id_pattern");
        let pat = if pat.is_empty() { r"^[a-z][a-z0-9_]*(\.[a-z0-9_<>-]+)+$".to_string() } else { pat };
        Ok(Adb {
            serial,
            package: t.str("adb", "package"),
            activity: t.str("adb", "activity"),
            reset_uri: t.str("launch", "reset"),
            ready: t.str("launch", "ready"),
            ready_timeout: t.get("launch", "timeout").and_then(Value::as_f64).unwrap_or(20.0),
            start: t.get("launch", "start").and_then(Value::as_bool).unwrap_or(true),
            release_only: t.get("adb", "release_only").and_then(Value::as_bool).unwrap_or(true),
            logcat: t.strs("evidence", "logcat"),
            id_pattern: Regex::new(&pat).map_err(|e| format!("[adb] id_pattern: {e}"))?,
            log_since: String::new(),
            deny: t.strs("adb", "deny"),
        })
    }

    fn run(&self, args: &[&str], timeout: f64) -> Result<Vec<u8>, String> {
        let mut child = Command::new("adb")
            .arg("-s")
            .arg(&self.serial)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("can't run adb: {e}"))?;
        // Read while it runs: a screenshot is megabytes, more than a pipe holds.
        let mut out = child.stdout.take().unwrap();
        let mut err = child.stderr.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut out, &mut buf);
            buf
        });
        let ereader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut err, &mut buf);
            buf
        });
        let t0 = Instant::now();
        let status = loop {
            if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
                break st;
            }
            if t0.elapsed().as_secs_f64() > timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("adb {} timed out after {timeout}s", args.join(" ")));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let stdout = reader.join().unwrap_or_default();
        let stderr = ereader.join().unwrap_or_default();
        if !status.success() {
            return Err(format!("adb {}: {}", args.join(" "), String::from_utf8_lossy(&stderr).trim()));
        }
        Ok(stdout)
    }

    fn shell(&self, cmd: &str) -> Result<String, String> {
        self.run(&["shell", cmd], 30.0).map(|o| String::from_utf8_lossy(&o).into_owned())
    }

    fn prop(&self, name: &str) -> String {
        self.shell(&format!("getprop {name}")).map(|s| s.trim().to_string()).unwrap_or_default()
    }

    /// The denied devices: serial, hardware serial or model matching a `deny` glob.
    fn denied(&self) -> Option<String> {
        let names = [self.serial.clone(), self.prop("ro.serialno"), self.prop("ro.product.model")];
        for g in &self.deny {
            if let Some(n) = names.iter().find(|n| !n.is_empty() && fnmatchcase(n, g)) {
                return Some(format!("{n} matches deny '{g}'"));
            }
        }
        None
    }

    fn window(&self) -> Result<(String, bool), String> {
        let out = self.shell("dumpsys window | grep -E 'mCurrentFocus|isKeyguardShowing|mShowingLockscreen'")?;
        let focus = Regex::new(r"mCurrentFocus=Window\{\S+ \S+ ([^}\s]+)\}").unwrap();
        let f = focus.captures(&out).map(|c| c[1].to_string()).unwrap_or_default();
        let locked = out.contains("isKeyguardShowing=true") || out.contains("mShowingLockscreen=true");
        Ok((f, locked))
    }

    /// Acting is allowed only on an unlocked device with the app in front.
    fn guard(&self) -> Result<(), String> {
        let (focus, locked) = self.window()?;
        if locked {
            return Err("the device is locked: verify never touches a lock screen".into());
        }
        if !focus.starts_with(&format!("{}/", self.package)) {
            return Err(format!("the app is not in front (focus: {})", if focus.is_empty() { "none" } else { &focus }));
        }
        Ok(())
    }

    fn pid(&self) -> Option<String> {
        self.shell(&format!("pidof {}", self.package)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    fn input(&self, args: &str) -> Result<(), String> {
        self.shell(&format!("input {args}")).map(|_| ())
    }

    fn open_uri(&self, uri: &str) -> Result<(), String> {
        if !Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*://[A-Za-z0-9/._~%?&=:+-]*$").unwrap().is_match(uri) {
            return Err(format!("open: '{uri}' is not a plain URI"));
        }
        let out = self.shell(&format!("am start -W -a android.intent.action.VIEW -d '{uri}' -p {}", self.package))?;
        if out.contains("Error") {
            return Err(format!("open {uri}: {}", out.trim()));
        }
        Ok(())
    }

    fn wait_for(&mut self, id: &str, timeout: f64) -> Result<Screen, String> {
        let t0 = Instant::now();
        loop {
            let s = self.observe()?;
            if s.find(id, &self.package).is_some() {
                return Ok(s);
            }
            if t0.elapsed().as_secs_f64() > timeout {
                return Err(format!("'{id}' did not appear within {timeout}s (on screen: {})", s.ids(&self.package).join(", ")));
            }
            std::thread::sleep(Duration::from_millis(400));
        }
    }
}

fn sendable(t: &str) -> Result<(), String> {
    if !t.chars().all(|c| c.is_ascii_alphanumeric() || " .,-_@".contains(c)) {
        return Err(format!("type: '{t}' has characters adb's input text can't send safely (letters, digits, space . , - _ @)"));
    }
    Ok(())
}

impl Driver for Adb {
    fn doctor(&mut self, checks: &[String]) -> Vec<Check> {
        let mut out = Vec::new();
        let mut add = |name: &str, r: Result<String, String>| {
            out.push(match r {
                Ok(d) => Check { name: name.into(), ok: true, detail: d },
                Err(d) => Check { name: name.into(), ok: false, detail: d },
            })
        };
        let state = self.run(&["get-state"], 10.0).map(|o| String::from_utf8_lossy(&o).trim().to_string());
        let reachable = matches!(&state, Ok(s) if s == "device");
        add(
            "device",
            match state {
                Ok(s) if s == "device" => match self.denied() {
                    Some(why) => Err(format!("{} is denied for verify ({why})", self.serial)),
                    None => Ok(format!("{} {}", self.serial, self.prop("ro.product.model"))),
                },
                Ok(s) => Err(format!("{} is {s}", self.serial)),
                Err(e) => Err(e),
            },
        );
        if !reachable {
            return out;
        }
        for c in checks {
            let r = match c.as_str() {
                "device" => continue,
                "awake" => self
                    .shell("dumpsys power | grep mWakefulness=")
                    .and_then(|s| if s.contains("Awake") { Ok("awake".into()) } else { Err(format!("screen is off ({})", s.trim())) }),
                "unlocked" => self.window().and_then(|(_, locked)| {
                    if locked {
                        Err("locked: unlock it by hand; verify never touches a lock screen".into())
                    } else {
                        Ok("unlocked".into())
                    }
                }),
                "package" => self.shell(&format!("pm path {}", self.package)).and_then(|s| {
                    if s.contains("package:") {
                        Ok(format!("{} installed", self.package))
                    } else {
                        Err(format!("{} is not installed", self.package))
                    }
                }),
                "release" => self.shell(&format!("dumpsys package {}", self.package)).and_then(|s| {
                    let v = Regex::new(r"versionName=(\S+)").unwrap().captures(&s).map(|c| c[1].to_string()).unwrap_or_default();
                    let flags = s.lines().find(|l| l.trim_start().starts_with("flags=[")).unwrap_or("").to_string();
                    if flags.contains("DEBUGGABLE") && self.release_only {
                        Err(format!("{v} is a debuggable build; this target drives release builds only"))
                    } else {
                        Ok(format!("release {v}"))
                    }
                }),
                "foreground" => self.window().map(|(f, _)| {
                    if f.starts_with(&format!("{}/", self.package)) {
                        "in front".into()
                    } else {
                        format!("not in front ({f}); launch brings it")
                    }
                }),
                other => Err(format!("unknown doctor check '{other}' (device, awake, unlocked, package, release, foreground)")),
            };
            add(c, r);
        }
        out
    }

    fn launch(&mut self, cold: bool) -> Result<bool, String> {
        let (_, locked) = self.window()?;
        if locked {
            return Err("the device is locked: verify never touches a lock screen".into());
        }
        if cold {
            self.shell(&format!("am force-stop {}", self.package))?;
            if self.pid().is_some() {
                return Err(format!("{} still runs after force-stop", self.package));
            }
        }
        self.log_since = self.shell("date +'%m-%d %H:%M:%S.000'")?.trim().to_string();
        // Cold: verify stopped it, so this launch is verify's even if the system brought it straight back (a HOME app).
        let running = !cold && self.pid().is_some();
        if self.start && !self.activity.is_empty() {
            let comp = if self.activity.contains('/') { self.activity.clone() } else { format!("{}/{}", self.package, self.activity) };
            let out = self.shell(&format!("am start -W -n {comp}"))?;
            if out.contains("Error") {
                return Err(format!("am start {comp}: {}", out.trim()));
            }
        }
        if !self.reset_uri.is_empty() {
            let uri = self.reset_uri.clone();
            std::thread::sleep(Duration::from_millis(600));
            self.open_uri(&uri)?;
        }
        self.guard()?;
        if !self.ready.is_empty() {
            let (ready, t) = (self.ready.clone(), self.ready_timeout);
            self.wait_for(&ready, t)?;
        }
        Ok(!running)
    }

    fn reset(&mut self) -> Result<(), String> {
        if self.reset_uri.is_empty() {
            return Ok(());
        }
        self.guard()?;
        let uri = self.reset_uri.clone();
        self.open_uri(&uri)?;
        if !self.ready.is_empty() {
            let (ready, t) = (self.ready.clone(), self.ready_timeout);
            self.wait_for(&ready, t)?;
        }
        Ok(())
    }

    fn observe(&mut self) -> Result<Screen, String> {
        let mut last = String::new();
        for _ in 0..4 {
            let raw = self.run(&["exec-out", "uiautomator", "dump", "/dev/tty"], 30.0)?;
            let text = String::from_utf8_lossy(&raw).into_owned();
            let xml = match text.find("</hierarchy>") {
                Some(i) => text[..i + "</hierarchy>".len()].to_string(),
                None => {
                    last = text.trim().chars().take(200).collect();
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };
            let mut s = tree::parse(&xml, &self.id_pattern)?;
            s.focus = self.window()?.0;
            return Ok(s);
        }
        Err(format!("uiautomator dump failed: {last}"))
    }

    fn act(&mut self, step: &Step, screen: &Screen) -> Result<(), String> {
        self.guard()?;
        let node = |id: &str| -> Result<[i64; 4], String> {
            match screen.find(id, &self.package) {
                // bounds are the visible part: a sliver under an edge is not something a person could press
                Some(n) if n.bounds[2] - n.bounds[0] >= 24 && n.bounds[3] - n.bounds[1] >= 24 => Ok(n.bounds),
                Some(n) => Err(format!("'{id}' is barely on screen ({:?}): scroll it into view first (a swipe step)", n.bounds)),
                None => Err(format!("no element '{id}' on screen (on screen: {})", screen.ids(&self.package).join(", "))),
            }
        };
        match step.action.as_str() {
            "tap" => {
                let (x, y) = tree::point(node(&step.arg)?, &step.at)?;
                self.input(&format!("tap {x} {y}"))
            }
            "long_press" => {
                let (x, y) = tree::point(node(&step.arg)?, &step.at)?;
                self.input(&format!("swipe {x} {y} {x} {y} 800"))
            }
            "type" => {
                sendable(&step.text)?;
                let (x, y) = tree::point(node(&step.arg)?, &step.at)?;
                self.input(&format!("tap {x} {y}"))?;
                std::thread::sleep(Duration::from_millis(500));
                // `pace = "burst"`: the whole text in one go. Otherwise a key at a time at a person's pace; both
                // must survive an app that re-renders its field mid-typing (Mob's Places bar lost keys to it).
                if step.pace == "burst" {
                    return self.input(&format!("text '{}'", step.text.replace(' ', "%s")));
                }
                for c in step.text.chars() {
                    let key = if c == ' ' { "%s".to_string() } else { c.to_string() };
                    self.input(&format!("text '{key}'"))?;
                    std::thread::sleep(Duration::from_millis(150));
                }
                Ok(())
            }
            "swipe" => {
                let b = node(&step.arg)?;
                let (cx, cy) = ((b[0] + b[2]) / 2, (b[1] + b[3]) / 2);
                let (w, h) = ((b[2] - b[0]) * 3 / 10, (b[3] - b[1]) * 3 / 10);
                let (x1, y1, x2, y2) = match step.dir.as_str() {
                    "up" | "" => (cx, cy + h, cx, cy - h),
                    "down" => (cx, cy - h, cx, cy + h),
                    "left" => (cx + w, cy, cx - w, cy),
                    "right" => (cx - w, cy, cx + w, cy),
                    other => return Err(format!("swipe dir '{other}' (up, down, left, right)")),
                };
                self.input(&format!("swipe {x1} {y1} {x2} {y2} 300"))
            }
            "back" => self.input("keyevent KEYCODE_BACK"),
            "key" => {
                if !step.arg.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
                    return Err(format!("key '{}': a KEYCODE name like ENTER", step.arg));
                }
                self.input(&format!("keyevent KEYCODE_{}", step.arg))
            }
            "open" => self.open_uri(&step.arg),
            other => Err(format!("{other} is not an action the adb driver performs")),
        }
    }

    fn screenshot(&mut self, path: &Path) -> Result<(), String> {
        let png = self.run(&["exec-out", "screencap", "-p"], 30.0)?;
        std::fs::write(path, png).map_err(|e| e.to_string())
    }

    fn log(&mut self) -> Result<String, String> {
        let mut args: Vec<String> = vec!["logcat".into(), "-d".into(), "-v".into(), "threadtime".into()];
        if !self.log_since.is_empty() {
            args.push("-T".into());
            args.push(self.log_since.clone());
        }
        if !self.logcat.is_empty() {
            args.extend(self.logcat.iter().cloned());
            args.push("*:S".into());
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run(&refs, 60.0).map(|o| String::from_utf8_lossy(&o).into_owned())
    }

    fn cleanup(&mut self, started: bool) -> Result<String, String> {
        if started {
            self.shell(&format!("am force-stop {}", self.package))?;
            return Ok(format!("stopped {} (verify started it)", self.package));
        }
        let back = if self.reset_uri.is_empty() { Ok(()) } else { self.reset() };
        back.map(|_| format!("left {} running (it was running before verify)", self.package))
    }

    fn package(&self) -> &str {
        &self.package
    }

    fn describe(&mut self) -> Value {
        let pkg = self.shell(&format!("dumpsys package {}", self.package)).unwrap_or_default();
        let grab = |k: &str| Regex::new(&format!(r"{k}=(\S+)")).unwrap().captures(&pkg).map(|c| c[1].to_string()).unwrap_or_default();
        json!({
            "driver": "adb",
            "device": self.serial,
            "model": self.prop("ro.product.model"),
            "package": self.package,
            "version": grab("versionName"),
            "version_code": grab("versionCode"),
        })
    }
}
