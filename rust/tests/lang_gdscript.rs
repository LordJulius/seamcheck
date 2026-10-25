//! The GDScript adapter and its resolution, ported from tests/test_gdscript.py
//! (`extends "relative.gd"`, `res://` paths from a Godot project in a
//! subfolder, a receiver declared as an engine class).

mod common;

use std::collections::BTreeSet;

use common::write;
use seamcheck::graph::Graph;
use seamcheck::project::Project;

const FILES: &[(&str, &str)] = &[
    ("demo/project.godot", "[application]\n"),
    ("demo/states/motion.gd", "extends Node\n\nfunc update_look() -> void:\n\tpass\n"),
    ("demo/states/ground/move.gd", "extends \"../motion.gd\"\n\nfunc enter() -> void:\n\tupdate_look()\n"),
    ("demo/states/other.gd", "extends Node\n\nfunc update_look() -> void:\n\tpass\n"),
    ("demo/ball.gd", "extends Area2D\n\nfunc reset() -> void:\n\tpass\n"),
    ("demo/label.gd", "extends Label\n\nfunc reset() -> void:\n\tpass\n"),
    ("demo/wall.gd", "extends Area2D\n\nfunc _on_entered(area: Area2D) -> void:\n\tarea.reset()\n"),
    ("demo/game.gd", "extends \"res://states/motion.gd\"\n\nfunc run() -> void:\n\tupdate_look()\n"),
];

fn load() -> (tempfile::TempDir, Graph) {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), FILES);
    write(d.path(), &[("seamcheck.toml", "[project]\nname = \"t\"\ninclude = [\"**\"]\n")]);
    let p = Project::load(d.path(), &[]);
    let g = Graph::load(&p);
    (d, g)
}

fn edges(g: &Graph, fid: &str) -> BTreeSet<(String, bool)> {
    g.edges.get(fid).map(|es| es.iter().map(|(t, _, e)| (t.clone(), *e)).collect()).unwrap_or_default()
}

fn one(callee: &str) -> BTreeSet<(String, bool)> {
    BTreeSet::from([(callee.to_string(), true)])
}

// test_gdscript.py: test_relative_extends
#[test]
fn relative_extends() {
    let (_d, g) = load();
    assert_eq!(edges(&g, "move.enter"), one("motion.update_look"));
}

// test_gdscript.py: test_res_path_from_a_subfolder_project
#[test]
fn res_path_from_a_subfolder_project() {
    let (_d, g) = load();
    assert_eq!(edges(&g, "game.run"), one("motion.update_look"));
}

// test_gdscript.py: test_engine_typed_receiver
#[test]
fn engine_typed_receiver() {
    let (_d, g) = load();
    assert_eq!(edges(&g, "wall._on_entered"), one("ball.reset"));
}
