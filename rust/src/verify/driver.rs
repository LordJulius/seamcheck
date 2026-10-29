//! What verify needs from a way of reaching an app. `adb` is the only driver
//! built; `http` (requests as acts, responses as states), `pty` (a terminal
//! app, its screen text as state) and `cdp` (a browser, the DOM by
//! `data-testid`) fit the same trait.

use std::path::Path;

use super::config::Step;
use super::tree::Screen;

pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

pub trait Driver {
    /// Read-only: is this instance worth driving? Never launches or touches anything.
    fn doctor(&mut self, checks: &[String]) -> Vec<Check>;
    /// Reach a usable state from empty. `cold`: stop the app first, so the run starts it. Returns whether
    /// verify started the app (cleanup stops only that).
    fn launch(&mut self, cold: bool) -> Result<bool, String>;
    /// Bring the app back to its root (a reset link), without starting or stopping it.
    fn reset(&mut self) -> Result<(), String>;
    /// The screen now.
    fn observe(&mut self) -> Result<Screen, String>;
    /// Do one step on the screen it was planned against (real input, by id).
    fn act(&mut self, step: &Step, screen: &Screen) -> Result<(), String>;
    fn screenshot(&mut self, path: &Path) -> Result<(), String>;
    /// The app's log since `launch`.
    fn log(&mut self) -> Result<String, String>;
    /// Stop the app if `started`; anything else verify found running is left as it was.
    fn cleanup(&mut self, started: bool) -> Result<String, String>;
    /// The app's package (whose nodes a state records).
    fn package(&self) -> &str;
    /// What was driven (device, app version), for the evidence.
    fn describe(&mut self) -> serde_json::Value;
}
