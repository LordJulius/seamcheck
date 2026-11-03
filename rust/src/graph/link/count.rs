//! The resolution counters (core.Project.resolution, core.Project.possible,
//! core._nothing): per file extension, how each call site resolved, counted
//! once per site (a call inside a nested function is counted where it is
//! made). `stats` prints them; the gate's exactness guard compares them.

use std::collections::HashMap;

use indexmap::IndexMap;

use super::shares_family;
use crate::graph::Graph;
use crate::model::{Call, Func};
use crate::project::ext_of;

/// extension -> [exact, guess only, nothing, nothing though a project function has the name]
pub type Resolution = IndexMap<String, [usize; 4]>;
/// name -> [(caller, line, type arguments)]: calls that reached no project
/// function though one has the name (a method on a library-typed value)
pub type Possible = HashMap<String, Vec<(String, usize, i64)>>;

#[derive(Default)]
pub(super) struct Counts {
    pub res: Resolution,
    pub possible: Possible,
}

/// A project function in the caller's language has this name (core._nothing's `named`).
pub(super) fn named(g: &Graph, file: &str, name: &str) -> bool {
    g.by_name
        .get(name)
        .map(|ids| ids.iter().filter_map(|id| g.func(id)).any(|h| h.kind != "top" && shares_family(file, &h.file)))
        .unwrap_or(false)
}

impl Counts {
    fn slot(&mut self, file: &str) -> &mut [usize; 4] {
        self.res.entry(ext_of(file).to_string()).or_insert([0; 4])
    }

    /// A call site's first resolution: exact, a name guess, or nothing.
    pub fn site(&mut self, g: &Graph, f: &Func, c: &Call, targets: &[(String, bool)]) {
        if targets.is_empty() {
            let named = named(g, &f.file, &c.name);
            let r = self.slot(&f.file);
            r[2] += 1;
            if named {
                r[3] += 1;
                self.possible.entry(c.name.clone()).or_default().push((f.id.clone(), c.line, c.targs));
            }
        } else {
            let exact = targets.iter().any(|t| t.1);
            self.slot(&f.file)[if exact { 0 } else { 1 }] += 1;
        }
    }

    /// core._param_receivers: a guess on a parameter settled (to `hit` or to nothing).
    pub fn settled_receiver(&mut self, g: &Graph, f: &Func, c: &Call, hit: bool) {
        let named = !hit && named(g, &f.file, &c.name);
        let r = self.slot(&f.file);
        if r[1] > 0 {
            r[1] -= 1;
            if hit {
                r[0] += 1;
            } else {
                r[2] += 1;
                if named {
                    r[3] += 1;
                }
            }
        }
    }

    /// core._param_flow: a call through a parameter reached what its callers pass.
    pub fn settled_flow(&mut self, f: &Func, had_guess: bool) {
        let r = self.slot(&f.file);
        if had_guess && r[1] > 0 {
            r[1] -= 1;
            r[0] += 1;
        } else if !had_guess && r[2] > 0 {
            r[2] -= 1;
            r[0] += 1;
        }
    }
}
