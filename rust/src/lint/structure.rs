//! Nesting, not lines: which element or block a position sits in
//! (structure.py). Two trees over a file's text, positions kept (bytes):
//! `jsx_tree` (JSX elements and `{...}` expression containers) and
//! `brace_tree` (`{ }` blocks and `( )` groups with the text that opens
//! them). Both are forgiving heuristics: an unbalanced file gives a
//! shallower tree, never an error.

use crate::pyre::strip;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// a JSX element
    El,
    /// a JSX `{...}` expression
    Expr,
    /// a `{` block
    Brace,
    /// a `(` group
    Paren,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub kind: Kind,
    pub start: usize,
    /// exclusive
    pub end: usize,
    /// JSX tag name ("" = fragment); brace header
    pub name: String,
    /// JSX: just past the opening tag's `>`
    pub head_end: usize,
    /// braces: where the header starts
    pub hstart: usize,
    pub parent: isize,
    pub children: Vec<usize>,
}

impl Node {
    fn new(kind: Kind, start: usize) -> Node {
        Node { kind, start, end: 0, name: String::new(), head_end: 0, hstart: 0, parent: -1, children: Vec::new() }
    }
}

/// Nodes in document (pre-)order.
#[derive(Default, Clone, Debug)]
pub struct Tree {
    pub nodes: Vec<Node>,
    starts: Vec<usize>,
}

impl std::ops::Index<usize> for Tree {
    type Output = Node;
    fn index(&self, i: usize) -> &Node {
        &self.nodes[i]
    }
}

impl Tree {
    fn seal(mut self) -> Tree {
        self.starts = self.nodes.iter().map(|n| n.start).collect();
        self
    }
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
    pub fn iter(&self) -> std::slice::Iter<'_, Node> {
        self.nodes.iter()
    }

    pub fn ancestors(&self, i: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut p = self.nodes[i].parent;
        while p >= 0 {
            out.push(p as usize);
            p = self.nodes[p as usize].parent;
        }
        out
    }

    /// The deepest node of one of `kinds` containing `pos` (-1: none).
    pub fn innermost(&self, pos: usize, kinds: &[Kind]) -> isize {
        let mut k = self.starts.partition_point(|&s| s <= pos) as isize - 1;
        while k >= 0 {
            let n = &self.nodes[k as usize];
            if kinds.contains(&n.kind) && n.start <= pos && pos < n.end {
                return k;
            }
            k = n.parent;
        }
        -1
    }
}

// --- JSX ----------------------------------------------------------------------------

#[derive(PartialEq, Eq, Clone, Copy)]
enum Sig {
    Empty,
    Kw,
    Id,
    Ch(char),
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `<([A-Za-z_$][\w$.:-]*|>)` at i: the name ("" for a fragment) and the match length.
fn tag_start(t: &str, i: usize) -> Option<(String, usize)> {
    let b = t.as_bytes();
    if b.get(i) != Some(&b'<') {
        return None;
    }
    let rest = &t[i + 1..];
    let mut chars = rest.char_indices();
    let (_, c0) = chars.next()?;
    if c0 == '>' {
        return Some((String::new(), 2));
    }
    if !(c0.is_ascii_alphabetic() || c0 == '_' || c0 == '$') {
        return None;
    }
    let mut end = c0.len_utf8();
    for (k, c) in chars {
        if is_word(c) || matches!(c, '$' | '.' | ':' | '-') {
            end = k + c.len_utf8();
        } else {
            break;
        }
    }
    Some((rest[..end].to_string(), 1 + end))
}

struct Jsx<'a> {
    t: &'a str,
    b: &'a [u8],
    n: usize,
    nodes: Vec<Node>,
}

impl<'a> Jsx<'a> {
    fn add(&mut self, mut node: Node, parent: isize) -> usize {
        node.parent = parent;
        self.nodes.push(node);
        let k = self.nodes.len() - 1;
        if parent >= 0 {
            self.nodes[parent as usize].children.push(k);
        }
        k
    }

    fn prev_sig(&self, i: usize) -> Sig {
        let mut j = i as isize - 1;
        while j >= 0 && matches!(self.b[j as usize], b' ' | b'\t' | b'\r' | b'\n') {
            j -= 1;
        }
        if j < 0 {
            return Sig::Empty;
        }
        let head = &self.t[..j as usize + 1];
        let c = head.chars().next_back().unwrap();
        if c.is_alphabetic() {
            let mut k = head.len();
            for (pos, ch) in head.char_indices().rev() {
                if is_word(ch) {
                    k = pos;
                } else {
                    break;
                }
            }
            let word = &head[k..];
            return if matches!(word, "return" | "yield" | "default" | "case" | "await") { Sig::Kw } else { Sig::Id };
        }
        Sig::Ch(c)
    }

    fn string(&mut self, i: usize) -> usize {
        let q = self.b[i];
        let mut j = i + 1;
        while j < self.n {
            let c = self.b[j];
            if c == b'\\' {
                j += 2;
                continue;
            }
            if c == q {
                return j + 1;
            }
            if c == b'\n' && q != b'`' {
                return j;
            }
            if q == b'`' && c == b'$' && self.b.get(j + 1) == Some(&b'{') {
                j = self.js(j + 2, b'}', -1) + 1;
                continue;
            }
            j += 1;
        }
        self.n
    }

    fn js(&mut self, mut i: usize, stop: u8, parent: isize) -> usize {
        let n = self.n;
        let mut want: Vec<u8> = Vec::new();
        while i < n {
            let c = self.b[i];
            if c == b'"' || c == b'\'' || c == b'`' {
                i = self.string(i);
                continue;
            }
            if c == b'/' && self.b.get(i + 1) == Some(&b'/') {
                i = self.t[i..].find('\n').map(|j| i + j).unwrap_or(n);
                continue;
            }
            if c == b'/' && self.b.get(i + 1) == Some(&b'*') {
                i = self.t[i + 2..].find("*/").map(|j| i + 2 + j + 2).unwrap_or(n);
                continue;
            }
            if c == b'/' && matches!(self.prev_sig(i), Sig::Ch(ch) if "(,=:[!&|?{};".contains(ch)) {
                let mut j = i + 1;
                let mut cls = false;
                while j < n && self.b[j] != b'\n' {
                    if self.b[j] == b'\\' {
                        j += 2;
                        continue;
                    }
                    if self.b[j] == b'[' {
                        cls = true;
                    } else if self.b[j] == b']' {
                        cls = false;
                    } else if self.b[j] == b'/' && !cls {
                        break;
                    }
                    j += 1;
                }
                i = j + 1;
                continue;
            }
            if c == b'<' && tag_start(self.t, i).is_some() {
                let sig = self.prev_sig(i);
                let ok = match sig {
                    Sig::Empty | Sig::Kw => true,
                    Sig::Ch(ch) => "(,=:?&|!{[;>}".contains(ch),
                    Sig::Id => false,
                };
                if ok {
                    i = self.element(i, parent);
                    continue;
                }
            }
            match c {
                b'(' => want.push(b')'),
                b'[' => want.push(b']'),
                b'{' => want.push(b'}'),
                _ => {
                    if !want.is_empty() && c == *want.last().unwrap() {
                        want.pop();
                    } else if want.is_empty() && c == stop {
                        return i;
                    }
                }
            }
            i += 1;
        }
        n
    }

    fn element(&mut self, i: usize, parent: isize) -> usize {
        let n = self.n;
        let (name, _) = tag_start(self.t, i).unwrap();
        let mut node = Node::new(Kind::El, i);
        node.name = name.clone();
        let k = self.add(node, parent);
        let mut j = i + 1 + name.len();
        if !name.is_empty() {
            while j < n {
                let c = self.b[j];
                if c == b'"' || c == b'\'' {
                    j = self.string(j);
                    continue;
                }
                if c == b'{' {
                    j = self.js(j + 1, b'}', k as isize) + 1;
                    continue;
                }
                if c == b'/' && self.b.get(j + 1) == Some(&b'>') {
                    self.nodes[k].head_end = j + 2;
                    self.nodes[k].end = j + 2;
                    return j + 2;
                }
                if c == b'>' {
                    break;
                }
                j += 1;
            }
        }
        self.nodes[k].head_end = j + 1;
        j += 1;
        while j < n {
            let c = self.b[j];
            if c == b'<' && self.b.get(j + 1) == Some(&b'/') {
                let e = self.t[j..].find('>').map(|e| j + e + 1).unwrap_or(n);
                self.nodes[k].end = e;
                return e;
            }
            if c == b'<' && tag_start(self.t, j).is_some() {
                j = self.element(j, k as isize);
                continue;
            }
            if c == b'{' {
                let x = self.add(Node::new(Kind::Expr, j), k as isize);
                let e = self.js(j + 1, b'}', x as isize);
                self.nodes[x].end = e + 1;
                j = e + 1;
                continue;
            }
            j += 1;
        }
        self.nodes[k].end = n;
        n
    }
}

/// JSX elements and expression containers of a JS/TS file.
pub fn jsx_tree(text: &str) -> Tree {
    let mut s = Jsx { t: text, b: text.as_bytes(), n: text.len(), nodes: Vec::new() };
    let mut i = 0;
    while i < s.n {
        let e = s.js(i, 0, -1);
        i = (e + 1).max(i + 1);
    }
    Tree { nodes: s.nodes, starts: Vec::new() }.seal()
}

/// The opening tag's text (`<button type="button" onClick={...}>`).
pub fn jsx_attrs<'t>(text: &'t str, node: &Node) -> &'t str {
    let a = node.start.min(text.len());
    let b = node.head_end.min(text.len()).max(a);
    &text[a..b]
}

// --- braces -------------------------------------------------------------------------

/// `{}` blocks and `()` groups of blanked code, each with its header.
pub fn brace_tree(code: &str) -> Tree {
    let b = code.as_bytes();
    let mut nodes: Vec<Node> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut bound: Vec<usize> = vec![0];
    for (i, &c) in b.iter().enumerate() {
        match c {
            b';' => {
                if stack.is_empty() || nodes[*stack.last().unwrap()].kind == Kind::Brace {
                    *bound.last_mut().unwrap() = i + 1;
                }
            }
            b'{' | b'(' => {
                let mut j = *bound.last().unwrap();
                if let Some(&top) = stack.last() {
                    if nodes[top].kind == Kind::Paren {
                        j = j.max(nodes[top].start + 1);
                        if j < i {
                            if let Some(k) = code[j..i].rfind(',') {
                                j = j + k + 1;
                            }
                        }
                    }
                }
                let kind = if c == b'{' { Kind::Brace } else { Kind::Paren };
                let mut node = Node::new(kind, i);
                node.name = if j <= i { strip(&code[j..i]).to_string() } else { String::new() };
                node.parent = stack.last().map(|&s| s as isize).unwrap_or(-1);
                node.hstart = j;
                nodes.push(node);
                let k = nodes.len() - 1;
                if let Some(&top) = stack.last() {
                    nodes[top].children.push(k);
                }
                stack.push(k);
                if c == b'{' {
                    bound.push(i + 1);
                }
            }
            b'}' | b')' => {
                if stack.is_empty() {
                    continue;
                }
                let want = if c == b'}' { Kind::Brace } else { Kind::Paren };
                for back in (0..stack.len()).rev() {
                    if nodes[stack[back]].kind == want {
                        for &k in &stack[back..] {
                            nodes[k].end = i + 1;
                            if nodes[k].kind == Kind::Brace {
                                bound.pop();
                            }
                        }
                        stack.truncate(back);
                        if c == b'}' {
                            *bound.last_mut().unwrap() = i + 1;
                        }
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    for k in stack {
        nodes[k].end = code.len();
    }
    Tree { nodes, starts: Vec::new() }.seal()
}

/// A header without its parenthesised groups (`Box(M.x(if (a) b))` -> `Box()`).
pub fn strip_groups(header: &str) -> String {
    let mut out = String::with_capacity(header.len());
    let mut depth = 0usize;
    for c in header.chars() {
        if c == '(' {
            if depth == 0 {
                out.push('(');
            }
            depth += 1;
        } else if c == ')' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                out.push(')');
            }
        } else if depth == 0 {
            out.push(c);
        }
    }
    out
}

/// The statement that opens a block: header without groups, last line only.
pub fn head_line(header: &str) -> String {
    let s = strip_groups(header);
    let s = s.trim_end_matches(|c: char| c.is_whitespace());
    let last = match s.rfind('\n') {
        Some(k) => &s[k + 1..],
        None => s,
    };
    strip(last).to_string()
}

/// A block's body without its nested blocks (QML: the object's own properties).
pub fn own_text(code: &str, tree: &Tree, k: usize) -> String {
    let n = &tree[k];
    let mut out = String::new();
    let mut pos = n.start + 1;
    for &c in &n.children {
        let ch = &tree[c];
        if ch.kind != Kind::Brace {
            continue;
        }
        if ch.start > pos {
            out.push_str(&code[pos..ch.start]);
        }
        pos = ch.end;
    }
    let e = n.end.saturating_sub(1);
    if e > pos {
        out.push_str(&code[pos..e]);
    }
    out
}
