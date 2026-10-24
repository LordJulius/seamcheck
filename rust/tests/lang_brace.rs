//! Brace-language adapter parse-level tests, ported from the Python suite.
//! Each case names its Python test; the expected summary was produced by
//! the Python adapter on the same text (module, function spans, calls).

mod common;

use std::collections::HashMap;
use std::path::Path;

use seamcheck::lang::{adapter_for, ParseCtx};

/// module / function summary of one file, as the Python adapter prints it
fn summary(rel: &str, text: &str) -> String {
    let res = HashMap::new();
    let ctx = ParseCtx { root: Path::new("/x"), res: &res };
    let mods = adapter_for(rel).expect("a brace adapter").parse(&ctx, rel, text);
    let mut out = String::new();
    for m in &mods {
        out.push_str(&format!("module {} kind={}\n", m.name, m.kind));
        for f in &m.funcs {
            let calls: Vec<String> = f
                .calls
                .iter()
                .map(|c| format!("{}{}@{}", if c.qual.is_empty() { String::new() } else { format!("{}.", c.qual) }, c.name, c.line))
                .collect();
            out.push_str(&format!("  {} {}-{} {} ret={} [{}]\n", f.name, f.start, f.end, f.kind, f.returns, calls.join(",")));
        }
    }
    out
}

// test_kotlin.py: test_expression_bodies_stop_at_their_own_end / test_declarations_without_bodies
#[test]
fn kotlin_spans() {
    let text = r####"
package x

object MapPrefs {
    fun on(values: Map<String, String>, key: String): Boolean = values[key] == "true"

    fun load(origin: String) {
        fetch(origin)
    }

    fun chain(a: Int): Int =
        a.plus(1)
            .times(2)

    fun set(k: String) { store(k) }
}

interface Repo {
    fun get(id: String): String
    fun put(id: String)
}
"####;
    let want = r####"module MapPrefs kind=class
  on 5-5 func ret=Boolean []
  load 7-9 func ret= [fetch@8]
  chain 11-13 func ret=Int [a.plus@12,..times@13]
  set 15-15 func ret= [store@15]
  <top> 1-22 top ret= []
module MapPrefs.Repo kind=interface
  get 19-19 func ret=String []
  put 20-20 func ret= []
"####;
    assert_eq!(summary("MapPrefs.kt", text), want);
}

// test_kotlin.py: test_long_signature_keeps_its_body
#[test]
fn kotlin_long_signature() {
    let text = r####"
package x

fun Screen(
    p0: () -> Unit = {},
p1: () -> Unit = {},
p2: () -> Unit = {},
p3: () -> Unit = {},
p4: () -> Unit = {},
p5: () -> Unit = {},
p6: () -> Unit = {},
p7: () -> Unit = {},
p8: () -> Unit = {},
p9: () -> Unit = {},
p10: () -> Unit = {},
p11: () -> Unit = {},
p12: () -> Unit = {},
p13: () -> Unit = {},
p14: () -> Unit = {},
p15: () -> Unit = {},
p16: () -> Unit = {},
p17: () -> Unit = {},
p18: () -> Unit = {},
p19: () -> Unit = {},
p20: () -> Unit = {},
p21: () -> Unit = {},
p22: () -> Unit = {},
p23: () -> Unit = {},
p24: () -> Unit = {},
p25: () -> Unit = {},
p26: () -> Unit = {},
p27: () -> Unit = {},
p28: () -> Unit = {},
p29: () -> Unit = {},
p30: () -> Unit = {},
p31: () -> Unit = {},
p32: () -> Unit = {},
p33: () -> Unit = {},
p34: () -> Unit = {},
p35: () -> Unit = {},
p36: () -> Unit = {},
p37: () -> Unit = {},
p38: () -> Unit = {},
p39: () -> Unit = {},
p40: () -> Unit = {},
p41: () -> Unit = {},
p42: () -> Unit = {},
p43: () -> Unit = {},
p44: () -> Unit = {},
p45: () -> Unit = {},
p46: () -> Unit = {},
p47: () -> Unit = {},
p48: () -> Unit = {},
p49: () -> Unit = {},
p50: () -> Unit = {},
p51: () -> Unit = {},
p52: () -> Unit = {},
p53: () -> Unit = {},
p54: () -> Unit = {},
p55: () -> Unit = {},
p56: () -> Unit = {},
p57: () -> Unit = {},
p58: () -> Unit = {},
p59: () -> Unit = {},
p60: () -> Unit = {},
p61: () -> Unit = {},
p62: () -> Unit = {},
p63: () -> Unit = {},
p64: () -> Unit = {},
p65: () -> Unit = {},
p66: () -> Unit = {},
p67: () -> Unit = {},
p68: () -> Unit = {},
p69: () -> Unit = {},
p70: () -> Unit = {},
p71: () -> Unit = {},
p72: () -> Unit = {},
p73: () -> Unit = {},
p74: () -> Unit = {},
p75: () -> Unit = {},
p76: () -> Unit = {},
p77: () -> Unit = {},
p78: () -> Unit = {},
p79: () -> Unit = {},
p80: () -> Unit = {},
p81: () -> Unit = {},
p82: () -> Unit = {},
p83: () -> Unit = {},
p84: () -> Unit = {},
p85: () -> Unit = {},
p86: () -> Unit = {},
p87: () -> Unit = {},
p88: () -> Unit = {},
p89: () -> Unit = {},
p90: () -> Unit = {},
p91: () -> Unit = {},
p92: () -> Unit = {},
p93: () -> Unit = {},
p94: () -> Unit = {},
p95: () -> Unit = {},
p96: () -> Unit = {},
p97: () -> Unit = {},
p98: () -> Unit = {},
p99: () -> Unit = {},
p100: () -> Unit = {},
p101: () -> Unit = {},
p102: () -> Unit = {},
p103: () -> Unit = {},
p104: () -> Unit = {},
p105: () -> Unit = {},
p106: () -> Unit = {},
p107: () -> Unit = {},
p108: () -> Unit = {},
p109: () -> Unit = {},
p110: () -> Unit = {},
p111: () -> Unit = {},
p112: () -> Unit = {},
p113: () -> Unit = {},
p114: () -> Unit = {},
p115: () -> Unit = {},
p116: () -> Unit = {},
p117: () -> Unit = {},
p118: () -> Unit = {},
p119: () -> Unit = {},
) {
    val t = { k: String -> MapPrefs.set(k) }
    helper()
}

fun helper() = MapPrefs.load("x")

fun Tile(icon: () -> Unit) {
    icon()
}
"####;
    let want = r####"module Screen kind=
  Screen 4-128 func ret= [MapPrefs.set@126,helper@127]
  helper 130-130 func ret=expr:MapPrefsload() [MapPrefs.load@130]
  Tile 132-134 func ret= [icon@133]
  <top> 1-135 top ret= []
"####;
    assert_eq!(summary("Screen.kt", text), want);
}

// test_kotlin.py: test_a_name_guess_is_never_exact
#[test]
fn kotlin_name_guess() {
    let text = r####"package x

fun Screen(
    p0: () -> Unit = {},
    p1: () -> Unit = {},
) {
    val t = { k: String -> MapPrefs.set(k) }
    helper()
}

fun helper() = MapPrefs.load("x")

fun Tile(icon: () -> Unit) {
    icon()
}
"####;
    let want = r####"module Screen kind=
  Screen 3-9 func ret= [MapPrefs.set@7,helper@8]
  helper 11-11 func ret=expr:MapPrefsload() [MapPrefs.load@11]
  Tile 13-15 func ret= [icon@14]
  <top> 1-16 top ret= []
"####;
    assert_eq!(summary("Screen.kt", text), want);
}

// test_typescript.py: test_generic_multiline_method_is_found
#[test]
fn ts_generic_method() {
    let text = r####"import type { ElementsMap } from "./types";

export class Scene {
  mutateElement<TElement extends Mutable<Element>>(
    element: TElement,
    updates: { a?: number; b?: string },
  ): TElement & {
    get(): TElement;
  } {
    return element as any;
  }

  getMap(): ElementsMap {
    return new Map();
  }
}
"####;
    let want = r####"module src/scene kind=class
  mutateElement 4-11 func ret=TElement [get@8]
  getMap 13-15 func ret=ElementsMap [Map@14]
  <top> 1-17 top ret= []
"####;
    assert_eq!(summary("src/scene.ts", text), want);
}

// test_typescript.py: test_window_augmentation_and_destructuring
#[test]
fn ts_app() {
    let text = r####"import { Scene } from "./scene";

export class App {
  scene: Scene = new Scene();
}

declare global {
  interface Window {
    h: {
      app: InstanceType<typeof App>;
      scene: Scene;
    };
  }
}
"####;
    let want = r####"module src/app kind=class
  <top> 1-15 top ret= [Scene@4]
module src/app.Window kind=interface
module src/app.Window.h kind=interface
"####;
    assert_eq!(summary("src/app.ts", text), want);
}

// test_go.py: test_every_receiver_is_typed
#[test]
fn go_methods() {
    let text = r####"package app

type Command struct {
	commands []*Command
}

func (c *Command) Name() string { return "" }

func (c *Command) Commands() []*Command { return c.commands }

type Other struct{}

func (o *Other) Name() string { return "" }
"####;
    let want = r####"module cmd kind=
  <top> 1-14 top ret= []
module cmd.Command kind=
  Name 7-7 func ret= []
  Commands 9-9 func ret=expr:ccommands []
module cmd.Other kind=
  Name 13-13 func ret= []
"####;
    assert_eq!(summary("cmd.go", text), want);
}

// test_go.py: test_every_receiver_is_typed
#[test]
fn go_calls() {
    let text = r####"package app

func TestNames(t *testing.T) {
	rootCmd.Name()
	for _, c := range rootCmd.Commands() {
		c.Name()
	}
	cmds := rootCmd.Commands()
	cmds[0].Name()
	b := new(Command)
	b.Name()
}
"####;
    let want = r####"module use_test kind=
  TestNames 3-12 test ret= [rootCmd.Name@4,rootCmd.Commands@5,c.Name@6,rootCmd.Commands@8,..Name@9,b.Name@11]
  <top> 1-13 top ret= []
"####;
    assert_eq!(summary("use_test.go", text), want);
}

// test_c_macros.py: test_gnu_style_definition_and_macro_call
#[test]
fn c_gnu_definition() {
    let text = r####"#include "jv.h"

int
yyparse (int answer)
{
  return JV_ARRAY(answer, 1);
}
"####;
    let want = r####"module src/parser kind=
  yyparse 4-7 func ret=expr:JV_ARRAY() [JV_ARRAY@6]
  <top> 1-8 top ret= []
"####;
    assert_eq!(summary("src/parser.c", text), want);
}

// test_c_macros.py: test_gnu_style_definition_and_macro_call
#[test]
fn c_macro_header() {
    let text = r####"#include "jv.h"

int jv_array(void) {
  return 0;
}

int jv_array_append(int x, int a, int b) {
  return x + a + b;
}
"####;
    let want = r####"module src/jv kind=
  jv_array 3-5 func ret= []
  jv_array_append 7-9 func ret= []
  <top> 1-10 top ret= []
"####;
    assert_eq!(summary("src/jv.c", text), want);
}

// test_java_overloads.py: test_overload_by_argument_count (parse level)
#[test]
fn java_overloads() {
    let text = r####"package p;
public class A {
    int f(int a) { return a; }
    int f(int a, int b) { return g(a) + b; }
    private int g(int x) { return x; }
    @Deprecated
    public List<String> names() { return names2(); }
}
"####;
    let want = r####"module A kind=class
  f 3-3 func ret=expr:a [g@4]
  g 5-5 func ret=expr:x []
  names 7-7 func ret=List`1 [names2@7]
  <top> 1-9 top ret= []
"####;
    assert_eq!(summary("A.java", text), want);
}

// test_csharp.py: test_generic_class_is_its_own_type (parse level)
#[test]
fn csharp_generic_class() {
    let text = r####"namespace N {
    public class Box<T> {
        public T Value { get; set; }
        public T Get() { return Value; }
        public Box<T> Self() => this.Get2().Wrap();
    }
}
"####;
    let want = r####"module Box kind=class
  Get 4-4 func ret=T []
  Self 5-5 func ret=T [this.Get2@5,..Wrap@5]
  <top> 1-8 top ret= []
"####;
    assert_eq!(summary("Box.cs", text), want);
}

// test_swift.py: test_labelled_closure_parameter (parse level)
#[test]
fn swift_closure() {
    let text = r####"import Foundation
struct S {
    func run(done: (Int) -> Void) {
        done(1)
        helper { v in print(v) }
    }
    func helper(_ f: (Int) -> Void) { f(2) }
}
"####;
    let want = r####"module A kind=
  <top> 1-9 top ret= []
module A.S kind=class
  run 3-6 func ret= [done@4]
  helper 7-7 func ret= [f@7]
"####;
    assert_eq!(summary("A.swift", text), want);
}

// (no single Python test: written for this port) rust spans
#[test]
fn rust_impl() {
    let text = r####"use std::fmt;
pub struct P { x: i32 }
impl P {
    pub fn new() -> P { P { x: 1 } }
    fn get(&self) -> i32 { self.x.max(0) }
}
fn free() { P::new().get(); }
"####;
    let want = r####"module src/lib kind=
  free 7-7 func ret= [P.new@7,..get@7]
  <top> 1-8 top ret= []
module src/lib.P kind=class
  new 4-4 func ret=P []
  get 5-5 func ret= [x.max@5]
"####;
    assert_eq!(summary("src/lib.rs", text), want);
}

// (no single Python test: written for this port) php spans
#[test]
fn php_class() {
    let text = r####"<?php
namespace App;
class A {
    public function f($x) { return $this->g($x); }
    private function g($y) { return strlen($y); }
}
"####;
    let want = r####"module A kind=class
  f 4-4 func ret=expr:$thisg() [$this.g@4]
  g 5-5 func ret=expr:strlen() [strlen@5]
  <top> 1-7 top ret= []
"####;
    assert_eq!(summary("A.php", text), want);
}

// (no single Python test: written for this port) QML functions in a component
#[test]
fn qml_component() {
    let text = r####"import QtQuick 2.0
Item {
    id: root
    function go(x) { return helper(x) }
    function helper(y) { return y }
    Component.onCompleted: go(1)
}
"####;
    let want = r####"module Main kind=class
  go 4-4 func ret=expr:helper() [helper@4]
  helper 5-5 func ret=expr:y []
  <top> 1-8 top ret= [Item.Item@2]
"####;
    assert_eq!(summary("Main.qml", text), want);
}
