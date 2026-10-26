//! The document's text as a rope: a balanced tree of string chunks.
//!
//! Every leaf holds a chunk of at most [`MAX_LEAF`] bytes, never cut inside a
//! UTF-8 sequence, and every inner node at most [`MAX_CHILDREN`] children, all
//! leaves at one depth. Each node caches its byte length and its count of
//! `'\n'`, so the byte where a line starts and the line a byte is on are found
//! by one walk from the root, and an edit rewrites one path. Nodes are shared
//! behind [`Arc`]: cloning a rope is a pointer copy, and an edit copies only
//! the nodes it changes that another clone still holds — a snapshot for a
//! background reader costs nothing until the text moves on.
//!
//! An edit inside one leaf that leaves it within bounds rewrites that leaf in
//! place, the common case of typing; any other is a split at both ends and a
//! join, which rebalance as they go.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

/// The most bytes a leaf holds.
pub const MAX_LEAF: usize = 1024;
/// The fewest bytes a leaf holds before a join merges it with its neighbour.
const MIN_LEAF: usize = MAX_LEAF / 2;
/// The most children an inner node holds.
pub const MAX_CHILDREN: usize = 16;
/// The fewest children an inner node holds before a join merges it.
const MIN_CHILDREN: usize = MAX_CHILDREN / 4;

/// What a node knows of the text under it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Summary {
    bytes: usize,
    /// The `'\n'`s under it.
    newlines: usize,
}

impl Summary {
    fn of(text: &str) -> Summary {
        Summary {
            bytes: text.len(),
            newlines: count_newlines(text.as_bytes()),
        }
    }

    fn add(self, other: Summary) -> Summary {
        Summary {
            bytes: self.bytes + other.bytes,
            newlines: self.newlines + other.newlines,
        }
    }
}

/// The `'\n'`s in `bytes`, eight bytes a step.
pub(crate) fn count_newlines(bytes: &[u8]) -> usize {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    const NL: u64 = LO * b'\n' as u64;
    let mut count = 0;
    let (words, rest) = bytes.as_chunks::<8>();
    for word in words {
        let w = u64::from_le_bytes(*word) ^ NL;
        // A zero byte of `w` is a newline of the word: set its high bit.
        let zero = !((w & !HI).wrapping_add(!HI) | w | !HI);
        count += zero.count_ones() as usize;
    }
    count + rest.iter().filter(|&&b| b == b'\n').count()
}

#[derive(Debug, Clone)]
struct Node {
    summary: Summary,
    /// Zero for a leaf.
    height: u8,
    body: Body,
}

#[derive(Debug, Clone)]
enum Body {
    Leaf(String),
    Inner(Vec<Arc<Node>>),
}

impl Node {
    fn leaf(text: String) -> Arc<Node> {
        Arc::new(Node {
            summary: Summary::of(&text),
            height: 0,
            body: Body::Leaf(text),
        })
    }

    fn empty() -> Arc<Node> {
        Node::leaf(String::new())
    }

    /// A node over `children`, all of one height; the child itself for one,
    /// an empty leaf for none.
    fn from_children(mut children: Vec<Arc<Node>>) -> Arc<Node> {
        match children.len() {
            0 => Node::empty(),
            1 => children.pop().unwrap_or_else(Node::empty),
            _ => Node::inner(children),
        }
    }

    /// An inner node over `children`, all of one height, whatever their
    /// number.
    fn inner(children: Vec<Arc<Node>>) -> Arc<Node> {
        debug_assert!(!children.is_empty());
        debug_assert!(children.iter().all(|c| c.height == children[0].height));
        let summary = children
            .iter()
            .fold(Summary::default(), |s, c| s.add(c.summary));
        Arc::new(Node {
            summary,
            height: children[0].height + 1,
            body: Body::Inner(children),
        })
    }

    fn len(&self) -> usize {
        self.summary.bytes
    }

    fn children(&self) -> &[Arc<Node>] {
        match &self.body {
            Body::Inner(children) => children,
            Body::Leaf(_) => &[],
        }
    }

    /// Whether it holds enough to stand beside a sibling without merging.
    fn is_ok_child(&self) -> bool {
        match &self.body {
            Body::Leaf(text) => text.len() >= MIN_LEAF,
            Body::Inner(children) => children.len() >= MIN_CHILDREN,
        }
    }
}

/// Joins two trees into one, rebalancing where their seam leaves a node
/// under- or overfull. All leaves of the result sit at one depth.
fn concat(a: Arc<Node>, b: Arc<Node>) -> Arc<Node> {
    if a.len() == 0 {
        return b;
    }
    if b.len() == 0 {
        return a;
    }
    let (ha, hb) = (a.height, b.height);
    match ha.cmp(&hb) {
        std::cmp::Ordering::Less => {
            let bc = b.children();
            if ha + 1 == hb && a.is_ok_child() {
                return merge_nodes(std::slice::from_ref(&a), bc);
            }
            let first = concat(a, bc[0].clone());
            if first.height + 1 == hb {
                merge_nodes(std::slice::from_ref(&first), &bc[1..])
            } else {
                merge_nodes(first.children(), &bc[1..])
            }
        }
        std::cmp::Ordering::Equal => {
            if a.is_ok_child() && b.is_ok_child() {
                return Node::inner(vec![a, b]);
            }
            if ha == 0 {
                return merge_leaves(&a, &b);
            }
            merge_nodes(a.children(), b.children())
        }
        std::cmp::Ordering::Greater => {
            let ac = a.children();
            let n = ac.len();
            if hb + 1 == ha && b.is_ok_child() {
                return merge_nodes(ac, std::slice::from_ref(&b));
            }
            let last = concat(ac[n - 1].clone(), b);
            if last.height + 1 == ha {
                merge_nodes(&ac[..n - 1], std::slice::from_ref(&last))
            } else {
                merge_nodes(&ac[..n - 1], last.children())
            }
        }
    }
}

/// One node over `a` and `b`, siblings of one height, or two under a new
/// parent when they are too many for one.
fn merge_nodes(a: &[Arc<Node>], b: &[Arc<Node>]) -> Arc<Node> {
    let n = a.len() + b.len();
    let all = || a.iter().chain(b).cloned();
    if n <= MAX_CHILDREN {
        return Node::inner(all().collect());
    }
    let half = n / 2;
    let left: Vec<_> = all().take(half).collect();
    let right: Vec<_> = all().skip(half).collect();
    Node::inner(vec![Node::inner(left), Node::inner(right)])
}

/// Two leaves joined: one when they fit in one, split evenly when they do
/// not.
fn merge_leaves(a: &Node, b: &Node) -> Arc<Node> {
    let (Body::Leaf(ta), Body::Leaf(tb)) = (&a.body, &b.body) else {
        unreachable!("merge_leaves on an inner node");
    };
    let mut text = String::with_capacity(ta.len() + tb.len());
    text.push_str(ta);
    text.push_str(tb);
    if text.len() <= MAX_LEAF {
        return Node::leaf(text);
    }
    let cut = floor_boundary(&text, text.len() / 2);
    let tail = text.split_off(cut);
    Node::inner(vec![Node::leaf(text), Node::leaf(tail)])
}

/// The greatest char boundary of `text` at or before `at`.
fn floor_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// `node` cut at byte `at`: what lies before and what lies after.
fn split(node: &Arc<Node>, at: usize) -> (Arc<Node>, Arc<Node>) {
    if at == 0 {
        return (Node::empty(), node.clone());
    }
    if at >= node.len() {
        return (node.clone(), Node::empty());
    }
    match &node.body {
        Body::Leaf(text) => (
            Node::leaf(text[..at].to_owned()),
            Node::leaf(text[at..].to_owned()),
        ),
        Body::Inner(children) => {
            let mut start = 0;
            for (i, child) in children.iter().enumerate() {
                let end = start + child.len();
                if at == start {
                    return (
                        Node::from_children(children[..i].to_vec()),
                        Node::from_children(children[i..].to_vec()),
                    );
                }
                if at < end {
                    let (l, r) = split(child, at - start);
                    let before = Node::from_children(children[..i].to_vec());
                    let after = Node::from_children(children[i + 1..].to_vec());
                    return (concat(before, l), concat(r, after));
                }
                start = end;
            }
            (node.clone(), Node::empty())
        }
    }
}

/// A tree over `text`, its leaves as full as they go.
fn build(text: &str) -> Arc<Node> {
    if text.len() <= MAX_LEAF {
        return Node::leaf(text.to_owned());
    }
    let pieces = text.len().div_ceil(MAX_LEAF);
    let target = text.len().div_ceil(pieces);
    let mut level = Vec::with_capacity(pieces + 1);
    let mut rest = text;
    while !rest.is_empty() {
        let cut = if rest.len() <= MAX_LEAF {
            rest.len()
        } else {
            // A boundary near the even share, never past a full leaf.
            let near = floor_boundary(rest, target.min(MAX_LEAF));
            if near == 0 {
                floor_boundary(rest, MAX_LEAF)
            } else {
                near
            }
        };
        level.push(Node::leaf(rest[..cut].to_owned()));
        rest = &rest[cut..];
    }
    while level.len() > 1 {
        let groups = level.len().div_ceil(MAX_CHILDREN);
        let size = level.len().div_ceil(groups);
        let mut next = Vec::with_capacity(groups);
        let mut nodes = level.into_iter().peekable();
        while nodes.peek().is_some() {
            next.push(Node::inner(nodes.by_ref().take(size).collect()));
        }
        level = next;
    }
    level.pop().unwrap_or_else(Node::empty)
}

/// Replaces `range` of the leaf under `node` with `text` in place, when the
/// range lies in one leaf and the leaf stays within bounds. Returns whether
/// it did.
fn edit_in_place(node: &mut Arc<Node>, range: Range<usize>, text: &str) -> bool {
    let node = Arc::make_mut(node);
    match &mut node.body {
        Body::Leaf(leaf) => {
            let grown = leaf.len() - (range.end - range.start) + text.len();
            if grown == 0 || grown > MAX_LEAF {
                return false;
            }
            let removed = count_newlines(&leaf.as_bytes()[range.clone()]);
            leaf.replace_range(range, text);
            node.summary.bytes = leaf.len();
            node.summary.newlines =
                node.summary.newlines - removed + count_newlines(text.as_bytes());
            true
        }
        Body::Inner(children) => {
            let last = children.len() - 1;
            let mut start = 0;
            for (i, child) in children.iter_mut().enumerate() {
                let end = start + child.len();
                // The child holding the whole range: the one it starts in, or
                // the last at the end of the text.
                if range.start >= start && range.end <= end && (range.start < end || i == last) {
                    let before = child.summary;
                    if !edit_in_place(child, range.start - start..range.end - start, text) {
                        return false;
                    }
                    let after = child.summary;
                    node.summary.bytes = node.summary.bytes - before.bytes + after.bytes;
                    node.summary.newlines =
                        node.summary.newlines - before.newlines + after.newlines;
                    return true;
                }
                start = end;
            }
            false
        }
    }
}

/// The document's text: see the module docs.
#[derive(Debug, Clone)]
pub struct Rope {
    root: Arc<Node>,
}

impl Default for Rope {
    fn default() -> Self {
        Rope {
            root: Node::empty(),
        }
    }
}

impl From<&str> for Rope {
    fn from(text: &str) -> Self {
        Rope { root: build(text) }
    }
}

impl From<String> for Rope {
    fn from(text: String) -> Self {
        Rope::from(text.as_str())
    }
}

impl Rope {
    /// An empty rope.
    pub fn new() -> Rope {
        Rope::default()
    }

    /// Its length in bytes.
    #[inline]
    pub fn len(&self) -> usize {
        self.root.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Its lines: one more than its `'\n'`s.
    #[inline]
    pub fn line_count(&self) -> usize {
        self.root.summary.newlines + 1
    }

    /// Replaces `range` with `text`. Both ends must be char boundaries.
    pub fn replace(&mut self, range: Range<usize>, text: &str) {
        let len = self.len();
        let range = range.start.min(len)..range.end.min(len).max(range.start.min(len));
        debug_assert!(self.is_char_boundary(range.start) && self.is_char_boundary(range.end));
        if range.is_empty() && text.is_empty() {
            return;
        }
        if text.len() <= MAX_LEAF && edit_in_place(&mut self.root, range.clone(), text) {
            return;
        }
        let (left, rest) = split(&self.root, range.start);
        let (_, right) = split(&rest, range.end - range.start);
        let middle = build(text);
        let mut root = concat(concat(left, middle), right);
        // Shed the single-child parents a join can leave on top.
        while root.children().len() == 1 {
            root = root.children()[0].clone();
        }
        self.root = root;
    }

    /// Inserts `text` at byte `at`.
    pub fn insert(&mut self, at: usize, text: &str) {
        self.replace(at..at, text);
    }

    /// Removes `range`.
    pub fn remove(&mut self, range: Range<usize>) {
        self.replace(range, "");
    }

    /// The leaf holding byte `at` and where it starts; at the end, the last
    /// leaf.
    pub fn chunk_at(&self, at: usize) -> (&str, usize) {
        let mut node = &*self.root;
        let mut start = 0;
        loop {
            match &node.body {
                Body::Leaf(text) => return (text, start),
                Body::Inner(children) => {
                    let last = children.len() - 1;
                    let mut i = 0;
                    while i < last && at >= start + children[i].len() {
                        start += children[i].len();
                        i += 1;
                    }
                    node = &children[i];
                }
            }
        }
    }

    /// The chunks covering `range`, each cut to it, in order.
    pub fn chunks(&self, range: Range<usize>) -> Chunks<'_> {
        let end = range.end.min(self.len());
        Chunks {
            rope: self,
            at: range.start.min(end),
            end,
        }
    }

    /// Whether byte `at` is a char boundary (the end is one).
    pub fn is_char_boundary(&self, at: usize) -> bool {
        if at >= self.len() {
            return at == self.len();
        }
        let (chunk, start) = self.chunk_at(at);
        chunk.is_char_boundary(at - start)
    }

    /// The byte at `at`, if there is one.
    pub fn byte(&self, at: usize) -> Option<u8> {
        if at >= self.len() {
            return None;
        }
        let (chunk, start) = self.chunk_at(at);
        chunk.as_bytes().get(at - start).copied()
    }

    /// The text of `range` as one string, borrowed when one chunk holds it.
    pub fn slice(&self, range: Range<usize>) -> Cow<'_, str> {
        let mut chunks = self.chunks(range.clone());
        let Some(first) = chunks.next() else {
            return Cow::Borrowed("");
        };
        if first.len() == range.end.min(self.len()) - range.start.min(self.len()) {
            return Cow::Borrowed(first);
        }
        let mut out = String::with_capacity(range.len());
        out.push_str(first);
        for chunk in chunks {
            out.push_str(chunk);
        }
        Cow::Owned(out)
    }

    /// The whole text.
    pub fn to_text(&self) -> String {
        self.slice(0..self.len()).into_owned()
    }

    /// The byte line `line` starts at; the end past the last line.
    pub fn line_start(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        if line > self.root.summary.newlines {
            return self.len();
        }
        // Past the `line`-th newline.
        let mut node = &*self.root;
        let mut start = 0;
        let mut left = line;
        loop {
            match &node.body {
                Body::Leaf(text) => {
                    let mut seen = 0;
                    for (i, b) in text.bytes().enumerate() {
                        if b == b'\n' {
                            seen += 1;
                            if seen == left {
                                return start + i + 1;
                            }
                        }
                    }
                    return start + text.len();
                }
                Body::Inner(children) => {
                    let mut next = &children[children.len() - 1];
                    for child in children {
                        if child.summary.newlines >= left {
                            next = child;
                            break;
                        }
                        left -= child.summary.newlines;
                        start += child.len();
                    }
                    node = next;
                }
            }
        }
    }

    /// The byte line `line` ends at, before its `'\n'`.
    pub fn line_end(&self, line: usize) -> usize {
        if line + 1 >= self.line_count() {
            return self.len();
        }
        self.line_start(line + 1) - 1
    }

    /// The bytes of line `line`, its `'\n'` left out.
    pub fn line_range(&self, line: usize) -> Range<usize> {
        let start = self.line_start(line);
        let end = if line + 1 >= self.line_count() {
            self.len()
        } else {
            // The next newline at or after the start.
            let mut end = start;
            for chunk in self.chunks(start..self.len()) {
                match chunk.bytes().position(|b| b == b'\n') {
                    Some(i) => {
                        end += i;
                        break;
                    }
                    None => end += chunk.len(),
                }
            }
            end
        };
        start..end
    }

    /// Line `line`'s text, its `'\n'` left out.
    pub fn line(&self, line: usize) -> Cow<'_, str> {
        self.slice(self.line_range(line))
    }

    /// The line byte `at` is on.
    pub fn line_of(&self, at: usize) -> usize {
        let at = at.min(self.len());
        let mut node = &*self.root;
        let mut start = 0;
        let mut line = 0;
        loop {
            match &node.body {
                Body::Leaf(text) => {
                    return line + count_newlines(&text.as_bytes()[..at - start]);
                }
                Body::Inner(children) => {
                    let last = children.len() - 1;
                    let mut i = 0;
                    while i < last && at >= start + children[i].len() {
                        start += children[i].len();
                        line += children[i].summary.newlines;
                        i += 1;
                    }
                    node = &children[i];
                }
            }
        }
    }

    /// The `'\n'`s in `range`.
    pub fn newlines_in(&self, range: Range<usize>) -> usize {
        self.chunks(range)
            .map(|c| count_newlines(c.as_bytes()))
            .sum()
    }

    /// Calls `f` with each line's text, its break left out, in order, in one
    /// pass over the chunks.
    pub fn for_each_line(&self, mut f: impl FnMut(&str)) {
        let mut carry = String::new();
        for chunk in self.chunks(0..self.len()) {
            let mut rest = chunk;
            while let Some(i) = rest.find('\n') {
                if carry.is_empty() {
                    f(&rest[..i]);
                } else {
                    carry.push_str(&rest[..i]);
                    f(&carry);
                    carry.clear();
                }
                rest = &rest[i + 1..];
            }
            carry.push_str(rest);
        }
        f(&carry);
    }

    /// The chars from byte `at` on.
    pub fn chars_from(&self, at: usize) -> impl Iterator<Item = char> + '_ {
        self.chunks(at..self.len()).flat_map(str::chars)
    }

    /// The chars before byte `at`, nearest first.
    pub fn chars_before(&self, at: usize) -> CharsBefore<'_> {
        CharsBefore {
            rope: self,
            at: at.min(self.len()),
            chunk: "".chars(),
        }
    }

    /// The tree's depth, leaves at zero.
    pub fn depth(&self) -> usize {
        self.root.height as usize
    }

    /// Checks the tree's invariants: summaries add up, leaves sit at one depth
    /// within bounds, nodes hold at most [`MAX_CHILDREN`]. For tests.
    #[doc(hidden)]
    pub fn check(&self) {
        fn walk(node: &Node) -> Summary {
            match &node.body {
                Body::Leaf(text) => {
                    assert_eq!(node.height, 0);
                    assert!(text.len() <= MAX_LEAF, "leaf of {}", text.len());
                    let s = Summary::of(text);
                    assert_eq!(s, node.summary);
                    s
                }
                Body::Inner(children) => {
                    assert!(!children.is_empty() && children.len() <= MAX_CHILDREN);
                    let mut s = Summary::default();
                    for child in children {
                        assert_eq!(child.height + 1, node.height);
                        assert!(child.len() > 0, "an empty child");
                        s = s.add(walk(child));
                    }
                    assert_eq!(s, node.summary);
                    s
                }
            }
        }
        walk(&self.root);
    }
}

/// The chunks of a range of a [`Rope`], from [`Rope::chunks`].
pub struct Chunks<'a> {
    rope: &'a Rope,
    at: usize,
    end: usize,
}

impl<'a> Iterator for Chunks<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if self.at >= self.end {
            return None;
        }
        let (chunk, start) = self.rope.chunk_at(self.at);
        let from = self.at - start;
        let to = (self.end - start).min(chunk.len());
        if to <= from {
            self.at = self.end;
            return None;
        }
        self.at = start + to;
        Some(&chunk[from..to])
    }
}

/// The chars before a byte of a [`Rope`], nearest first, from
/// [`Rope::chars_before`].
pub struct CharsBefore<'a> {
    rope: &'a Rope,
    at: usize,
    chunk: std::str::Chars<'a>,
}

impl Iterator for CharsBefore<'_> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        loop {
            if let Some(c) = self.chunk.next_back() {
                return Some(c);
            }
            if self.at == 0 {
                return None;
            }
            let (chunk, start) = self.rope.chunk_at(self.at - 1);
            self.chunk = chunk[..self.at - start].chars();
            self.at = start;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }

        fn below(&mut self, n: usize) -> usize {
            if n == 0 { 0 } else { self.next() as usize % n }
        }
    }

    fn boundary(text: &str, at: usize) -> usize {
        floor_boundary(text, at)
    }

    fn piece(rng: &mut Lcg) -> String {
        const BITS: [&str; 8] = ["a", "bc", "\n", "é", "日本", "x\ny", "🦀", "  "];
        let n = rng.below(40);
        (0..n).map(|_| BITS[rng.below(BITS.len())]).collect()
    }

    fn agrees(rope: &Rope, model: &str) {
        rope.check();
        assert_eq!(rope.len(), model.len());
        assert_eq!(rope.to_text(), model);
        let lines: Vec<&str> = model.split('\n').collect();
        assert_eq!(rope.line_count(), lines.len());
        let mut start = 0;
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(rope.line_start(i), start, "start of line {i}");
            assert_eq!(rope.line(i), *line, "line {i}");
            assert_eq!(rope.line_of(start), i);
            start += line.len() + 1;
        }
    }

    #[test]
    fn newlines_are_counted_a_word_at_a_time() {
        let text = "a\nbc\n\n\nlong line without one\n0123456\n7";
        assert_eq!(count_newlines(text.as_bytes()), 6);
        assert_eq!(count_newlines(&[b'\n'; 17]), 17);
        assert_eq!(count_newlines(&[0x8a; 16]), 0);
    }

    #[test]
    fn a_large_text_builds_a_balanced_tree() {
        let line = "fn main() { println!(\"hello\"); }\n";
        let text = line.repeat(20_000);
        let rope = Rope::from(text.as_str());
        rope.check();
        assert_eq!(rope.line_count(), 20_001);
        assert_eq!(rope.line_start(12_345), 12_345 * line.len());
        assert_eq!(rope.line_of(12_345 * line.len() + 3), 12_345);
        assert!(rope.depth() <= 4, "depth {}", rope.depth());
    }

    #[test]
    fn random_edits_agree_with_a_string() {
        let mut rng = Lcg(7);
        for round in 0..16 {
            let mut model: String = (0..rng.below(4000)).map(|_| piece(&mut rng)).collect();
            if round % 5 == 0 {
                model = model.repeat(3);
            }
            let mut rope = Rope::from(model.as_str());
            agrees(&rope, &model);
            for _ in 0..60 {
                let a = boundary(&model, rng.below(model.len() + 1));
                let b = boundary(
                    &model,
                    a + {
                        let reach = if rng.below(4) == 0 { 6000 } else { 30 };
                        rng.below(reach)
                    },
                );
                let text = if rng.below(3) == 0 {
                    piece(&mut rng).repeat(rng.below(80) + 1)
                } else {
                    piece(&mut rng)
                };
                model.replace_range(a..b, &text);
                rope.replace(a..b, &text);
                rope.check();
                assert_eq!(rope.len(), model.len());
            }
            agrees(&rope, &model);
        }
    }

    #[test]
    fn typing_char_by_char_and_deleting_it_back_keeps_the_tree_sound() {
        let mut rope = Rope::new();
        let mut model = String::new();
        let text = "struct Point { x: f32, y: f32 }\n// 日本語 🦀\n".repeat(200);
        for c in text.chars() {
            let at = model.len();
            let mut buf = [0; 4];
            rope.insert(at, c.encode_utf8(&mut buf));
            model.push(c);
        }
        agrees(&rope, &model);
        while !model.is_empty() {
            let start = boundary(&model, model.len() / 2);
            let end = boundary(&model, start + 7).max(start + 1);
            let end = (start..=model.len())
                .find(|&e| e >= end && model.is_char_boundary(e))
                .unwrap_or(model.len());
            model.replace_range(start..end, "");
            rope.remove(start..end);
        }
        agrees(&rope, &model);
        assert_eq!(rope.depth(), 0);
    }

    #[test]
    fn a_clone_is_a_snapshot_the_original_moves_on_from() {
        let mut rope = Rope::from("one\ntwo\nthree".repeat(500).as_str());
        let snapshot = rope.clone();
        rope.insert(4, "and a half\n");
        assert_eq!(snapshot.line(1), "two");
        assert_eq!(rope.line(1), "and a half");
        snapshot.check();
    }

    #[test]
    fn chars_walk_both_ways_across_chunks() {
        let text = "αβγ\n".repeat(600);
        let rope = Rope::from(text.as_str());
        let mid = boundary(&text, text.len() / 2);
        let forward: String = rope.chars_from(mid).collect();
        assert_eq!(forward, &text[mid..]);
        let back: String = rope.chars_before(mid).collect();
        let want: String = text[..mid].chars().rev().collect();
        assert_eq!(back, want);
        assert!(rope.is_char_boundary(mid));
        assert!(!rope.is_char_boundary(1));
        assert_eq!(rope.slice(2..9), &text[2..9]);
    }

    #[test]
    fn line_ranges_leave_the_newline_out() {
        let rope = Rope::from("ab\n\ncd");
        assert_eq!(rope.line_range(0), 0..2);
        assert_eq!(rope.line_range(1), 3..3);
        assert_eq!(rope.line_range(2), 4..6);
        assert_eq!(rope.line_end(0), 2);
        assert_eq!(rope.line_start(9), 6);
    }
}
