//! Flask-SQLAlchemy and Flask-Login values the type walk can name (infer_flask.py).
//!
//! `User.query` on a Flask-SQLAlchemy model is a query of Users: builders keep
//! it one, one-row readers give a User, anything else is the library's. It
//! travels as an `Ext` value tagged with the model (`ROWS`), so every other
//! consumer treats it as the library's. Flask-Login's `current_user` is the
//! project's one `UserMixin` class.

use super::{esc, ext, re, rxd, Types, Val};
use crate::model::Module;

pub const ROWS: &str = "\0rows";
const BUILDERS: &[&str] = &[
    "filter()", "filter_by()", "order_by()", "group_by()", "having()", "join()", "outerjoin()", "options()", "limit()",
    "offset()", "distinct()", "where()", "select_from()", "params()", "execution_options()", "with_for_update()",
    "populate_existing()", "yield_per()", "enable_eagerloads()", "all()",
];
const ONE: &[&str] =
    &["first()", "one()", "one_or_none()", "get()", "get_or_404()", "first_or_404()", "one_or_404()", "scalar()"];

pub fn rows(mods: &[String]) -> Val {
    let mut v = vec![ROWS.to_string()];
    v.extend(mods.iter().cloned());
    Val::Ext(v)
}

pub fn is_rows(v: &Option<Val>) -> bool {
    matches!(v, Some(Val::Ext(m)) if m.first().map(|s| s == ROWS).unwrap_or(false))
}

/// A segment after a model's query.
pub fn step(v: &Val, seg: &str) -> Val {
    let mods: Vec<String> = v.mods()[1..].to_vec();
    if ONE.contains(&seg) {
        let one = mods.len() == 1;
        return Val::Inst(mods, one);
    }
    if BUILDERS.contains(&seg) {
        v.clone()
    } else {
        ext()
    }
}

/// A Python class module's supertypes as its `class X(...)` line writes them.
pub fn class_bases(m: &Module) -> Vec<String> {
    if m.decls.is_empty() || !crate::model::is_py(&m.file) {
        return Vec::new();
    }
    let head_rx = rxd(&format!(r"\s*class\s+{}\s*\(", esc(&m.decls[0])));
    let Some(head) = m.raw.iter().find(|ln| head_rx.match_start(ln).is_some()) else { return Vec::new() };
    match re!(r"\s*class\s+\w+\s*\(([^)]*)\)").match_start(head) {
        Some(bm) => bm.s(1).split(',').map(|x| x.trim()).filter(|x| !x.is_empty() && !x.contains('=')).map(String::from).collect(),
        None => Vec::new(),
    }
}

impl<'a> Types<'a> {
    /// Any of these classes is a Flask-SQLAlchemy model (a `db.Model` among its supertypes).
    pub fn flask_model(&self, mods: &[String]) -> bool {
        mods.iter().any(|mn| {
            self.ancestors(mn).iter().any(|a| {
                class_bases(self.m(a)).iter().any(|b| re!(r"^(?:[A-Za-z_]\w*\.)?Model$").match_start(b).is_some())
            })
        })
    }

    /// `current_user` imported from flask_login: an instance of the project's one UserMixin class.
    pub fn flask_login_user(&self, fm: &Module) -> Option<Val> {
        let cu = re!(r"^\s*from\s+flask_login\s+import\s+[^\n]*\bcurrent_user\b");
        if !fm.raw.iter().any(|ln| cu.match_start(ln).is_some()) {
            return None;
        }
        if let Some(hit) = &self.c.borrow().flask_user {
            return hit.clone();
        }
        let users: Vec<String> = self
            .g
            .modules
            .iter()
            .filter(|(_, m)| !m.owner.is_empty() && class_bases(m).iter().any(|b| b.rsplit('.').next() == Some("UserMixin")))
            .map(|(mn, _)| mn.clone())
            .collect();
        let got = if users.len() == 1 { Some(Val::Inst(users, true)) } else { None };
        self.c.borrow_mut().flask_user = Some(got.clone());
        got
    }
}
