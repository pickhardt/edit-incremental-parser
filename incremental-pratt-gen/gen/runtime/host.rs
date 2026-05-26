//! Generated statement-grammar front-end (GENERATED-CRATE RUNTIME,
//! grammar-driven).
//!
//! This is the "true front-end" layer: a *generated* statement/declaration
//! grammar wrapping the generated Pratt expression core, with edit-incremental
//! reuse across the whole program tree. The statement grammar itself is
//! emitted by the generator (`crate::host_grammar::GRAMMAR`) from the spec's
//! `[host]` section; this module is the grammar-independent interpreter +
//! incremental-reuse engine, byte-identical across grammars.
//!
//! Two reuse mechanisms compose into one tree:
//!
//!   * **statement layer — reparseable-element reuse.** A subtree whose source
//!     lies entirely outside the edited range is reused wholesale (`Arc::clone`).
//!     This is the mechanism IntelliJ / Roslyn / SwiftSyntax use; it is *not*
//!     the precedence-band predicate (statement grammars are not
//!     precedence-driven). Reuse recurses: an edit deep inside nested blocks
//!     reparses only the innermost enclosing production whose boundary token is
//!     unchanged, escalating one level at a time and bottoming out at a sound
//!     full reparse.
//!   * **expression layer — precedence-bounded reuse.** At an `expr` leaf the
//!     edited expression is reused via the verified `incremental_parse`
//!     predicate.
//!
//! Soundness rests on three guards, each re-validated against the actual source
//! and backstopped by the `host_incremental_matches_fresh` oracle (incremental
//! == fresh, over random multi-edit chains and nested blocks):
//!   1. the edit lies inside a single child's span;
//!   2. a reparsed subtree consumes *exactly* its delta-adjusted span (its
//!      following boundary token sits where the untouched suffix expects it —
//!      the one-token-lookahead guard, made exact); and
//!   3. for an expression leaf, the replacement introduces no host token bytes
//!      (so it cannot change statement structure).
//! Any guard failing escalates; the root escalation is a full fresh parse,
//! which is always sound.
//!
//! All widths are **relative** (each node stores its own byte width, not an
//! absolute span), so reusing the untouched suffix needs no position fix-up —
//! the same relative-offset discipline the expression, token, and rope layers
//! use. That is what makes repeated edits (chains) sound, which fixed-span
//! demonstrations cannot support.

use std::sync::Arc;

use crate::ast::Node;
use crate::edit::Edit;
use crate::host_grammar::GRAMMAR;
use crate::incremental::incremental_parse;
use crate::parser::{parse, ParseError};

// ---- Grammar table (built by the emitted `host_grammar.rs`) ------------

/// One term in an alternative's right-hand side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Term {
    /// A literal token (keyword or punctuation), e.g. `"let"`, `";"`, `"{"`.
    Tok(&'static str),
    /// An identifier atom (`[A-Za-z_][A-Za-z0-9_]*`, not a keyword).
    Ident,
    /// An embedded Pratt expression, delimited by the following `Tok` (or EOF
    /// at the top level).
    Expr,
    /// A reference to production `usize`.
    Nt(usize),
    /// Zero or more of production `usize`, until the following `Tok` (or EOF).
    Rep(usize),
}

#[derive(Debug, Clone, Copy)]
pub struct Alt {
    pub terms: &'static [Term],
}

#[derive(Debug, Clone, Copy)]
pub struct Production {
    pub name: &'static str,
    pub alts: &'static [Alt],
}

#[derive(Debug, Clone, Copy)]
pub struct Grammar {
    pub prods: &'static [Production],
    /// Index of the start production.
    pub start: usize,
    /// Reserved words (an `Ident` term will not match these).
    pub keywords: &'static [&'static str],
    /// Bytes appearing in punctuation literals — the expression-reuse guard
    /// rejects a replacement containing any of them.
    pub punct_bytes: &'static [u8],
}

// ---- Concrete syntax tree ----------------------------------------------

/// A child of a CST node. Widths are **relative**: `width` is the byte span
/// this child occupies, *including* the whitespace that precedes it (leading
/// trivia), so a node's width is exactly the sum of its children's widths and
/// absolute positions are recovered by a single additive walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    pub width: u32,
    pub node: ChildNode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildNode {
    /// A literal token (keyword/punctuation); the stored text is its spelling.
    Tok(Arc<str>),
    /// An identifier; the stored text is its spelling.
    Id(Arc<str>),
    /// An embedded Pratt expression subtree.
    Expr(Arc<Node>),
    /// A nonterminal subtree.
    Sub(Arc<Tree>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    pub prod: usize,
    pub alt: usize,
    pub kids: Vec<Child>,
    /// Total byte width (sum of `kids` widths).
    pub width: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub root: Tree,
}

#[derive(Debug, Default, Clone)]
pub struct HostStats {
    /// Subtrees reused wholesale (`Arc::clone`, outside the edit).
    pub stmts_reused: u32,
    /// Productions reparsed from source (the innermost enclosing the edit, plus
    /// any escalations).
    pub stmts_reparsed: u32,
    /// Expression subtrees reused inside the one edited expression leaf.
    pub exprs_reused: u32,
    /// True when reuse escalated all the way to a full fresh program parse.
    pub fell_back: bool,
    /// Depth of the innermost reparsed production (0 = root) — evidence reuse
    /// recurses into nested blocks rather than reparsing the whole program.
    pub reuse_depth: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    Expr(ParseError),
    Expected(&'static str, u32),
    Trailing(u32),
}

// ---- Normalized rendering (for oracle comparison) ----------------------

impl Program {
    /// Structural rendering independent of whitespace and byte positions
    /// (expressions via `unparse_normalized`). Two programs render equal iff
    /// they have the same statement/expression structure.
    pub fn unparse(&self) -> String {
        let mut s = String::new();
        unparse_tree(&self.root, &mut s);
        s
    }

    /// Absolute span of the first embedded expression in the tree, if any
    /// (depth-first). Used by the reuse demonstration.
    pub fn first_expr_span(&self) -> Option<(u32, u32)> {
        fn walk(t: &Tree, base: u32) -> Option<(u32, u32)> {
            let mut pos = base;
            for k in &t.kids {
                match &k.node {
                    ChildNode::Expr(_) => return Some((pos, pos + k.width)),
                    ChildNode::Sub(sub) => {
                        if let Some(r) = walk(sub, pos) {
                            return Some(r);
                        }
                    }
                    _ => {}
                }
                pos += k.width;
            }
            None
        }
        walk(&self.root, 0)
    }
}

fn unparse_tree(t: &Tree, out: &mut String) {
    for k in &t.kids {
        match &k.node {
            ChildNode::Tok(s) | ChildNode::Id(s) => {
                out.push_str(s);
                out.push(' ');
            }
            ChildNode::Expr(n) => {
                out.push_str(&n.unparse_normalized());
                out.push(' ');
            }
            ChildNode::Sub(sub) => unparse_tree(sub, out),
        }
    }
}

// ---- Fresh recursive-descent parse over the grammar table --------------

struct HostParser<'a> {
    b: &'a [u8],
    s: &'a str,
    pos: usize,
}

fn is_ident_byte(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

impl<'a> HostParser<'a> {
    fn new(s: &'a str, pos: usize) -> Self {
        HostParser { b: s.as_bytes(), s, pos }
    }

    /// Whitespace run length at `pos` (does not consume).
    fn ws_len(&self) -> usize {
        let mut i = self.pos;
        while i < self.b.len() && matches!(self.b[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        }
        i - self.pos
    }

    /// Does literal `lit` occur at byte `i` (after the run of whitespace),
    /// with a word boundary if `lit` is alphanumeric (so `let` != `lethal`)?
    fn literal_at(&self, i: usize, lit: &str) -> bool {
        let k = lit.as_bytes();
        if i + k.len() > self.b.len() || &self.b[i..i + k.len()] != k {
            return false;
        }
        let alnum = k.iter().all(|c| is_ident_byte(*c));
        if alnum {
            i + k.len() >= self.b.len() || !is_ident_byte(self.b[i + k.len()])
        } else {
            true
        }
    }

    /// Choose the alternative of production `p` whose FIRST token matches the
    /// lookahead (longest match, resolving nonterminal heads); else the unique
    /// alternative with a non-literal (open) head.
    fn choose_alt(&self, p: &Production) -> Result<usize, HostError> {
        let head = self.pos + self.ws_len();
        let mut best: Option<(usize, usize)> = None; // (alt index, matched len)
        let mut default: Option<usize> = None;
        for (ai, alt) in p.alts.iter().enumerate() {
            if let Some(len) = self.head_match(alt.terms.first(), head, 0) {
                if best.map_or(true, |(_, l)| len > l) {
                    best = Some((ai, len));
                }
            } else if Self::head_is_open(alt.terms.first(), 0) {
                default = Some(ai);
            }
        }
        if let Some((ai, _)) = best {
            return Ok(ai);
        }
        default.ok_or(HostError::Expected("statement", head as u32))
    }

    /// If the alternative led by `term` can begin with a literal that matches
    /// the source at `at`, return the matched byte length (longest), resolving
    /// a nonterminal head through its production's alternatives.
    fn head_match(&self, term: Option<&Term>, at: usize, depth: usize) -> Option<usize> {
        if depth > GRAMMAR.prods.len() {
            return None; // cycle guard
        }
        match term? {
            Term::Tok(lit) if self.literal_at(at, lit) => Some(lit.len()),
            Term::Tok(_) => None,
            Term::Nt(i) | Term::Rep(i) => GRAMMAR.prods[*i]
                .alts
                .iter()
                .filter_map(|a| self.head_match(a.terms.first(), at, depth + 1))
                .max(),
            Term::Ident | Term::Expr => None,
        }
    }

    /// Whether an alternative led by `term` can start without a leading literal
    /// (so it serves as the production's default arm).
    fn head_is_open(term: Option<&Term>, depth: usize) -> bool {
        if depth > GRAMMAR.prods.len() {
            return false;
        }
        match term {
            None => true, // empty alternative matches anything
            Some(Term::Ident) | Some(Term::Expr) | Some(Term::Rep(_)) => true,
            Some(Term::Tok(_)) => false,
            Some(Term::Nt(i)) => {
                GRAMMAR.prods[*i].alts.iter().any(|a| Self::head_is_open(a.terms.first(), depth + 1))
            }
        }
    }

    /// The literal that terminates an `Expr` / `Rep` term at index `ti` in
    /// `terms`: the next `Tok` after it, or `None` for EOF (top level).
    fn terminator(terms: &[Term], ti: usize) -> Option<&'static str> {
        match terms.get(ti + 1) {
            Some(Term::Tok(lit)) => Some(lit),
            None => None,
            _ => None, // grammar validation forbids non-Tok following Expr/Rep
        }
    }

    /// Parse production `p_idx`, returning its subtree (whose width includes
    /// leading whitespace consumed at each child).
    fn parse_prod(&mut self, p_idx: usize) -> Result<Tree, HostError> {
        let p = &GRAMMAR.prods[p_idx];
        let ai = self.choose_alt(p)?;
        let alt = &p.alts[ai];
        let start = self.pos;
        let mut kids: Vec<Child> = Vec::new();
        for (ti, term) in alt.terms.iter().enumerate() {
            match term {
                Term::Tok(lit) => {
                    let cstart = self.pos;
                    let ws = self.ws_len();
                    let at = self.pos + ws;
                    if !self.literal_at(at, lit) {
                        return Err(HostError::Expected(lit, at as u32));
                    }
                    self.pos = at + lit.len();
                    kids.push(Child {
                        width: (self.pos - cstart) as u32,
                        node: ChildNode::Tok(Arc::from(*lit)),
                    });
                }
                Term::Ident => {
                    let cstart = self.pos;
                    let ws = self.ws_len();
                    let at = self.pos + ws;
                    let mut j = at;
                    while j < self.b.len() && is_ident_byte(self.b[j]) {
                        j += 1;
                    }
                    if j == at || GRAMMAR.keywords.contains(&&self.s[at..j]) {
                        return Err(HostError::Expected("identifier", at as u32));
                    }
                    let name: Arc<str> = Arc::from(&self.s[at..j]);
                    self.pos = j;
                    kids.push(Child { width: (self.pos - cstart) as u32, node: ChildNode::Id(name) });
                }
                Term::Expr => {
                    let cstart = self.pos;
                    let end = self.scan_to(Self::terminator(alt.terms, ti));
                    let node = parse(&self.s[cstart..end]).map_err(HostError::Expr)?;
                    self.pos = end;
                    kids.push(Child { width: (end - cstart) as u32, node: ChildNode::Expr(node) });
                }
                Term::Nt(i) => {
                    let sub = self.parse_prod(*i)?;
                    kids.push(Child { width: sub.width, node: ChildNode::Sub(Arc::new(sub)) });
                }
                Term::Rep(i) => {
                    let term_lit = Self::terminator(alt.terms, ti);
                    loop {
                        let at = self.pos + self.ws_len();
                        if at >= self.b.len() {
                            break;
                        }
                        if let Some(lit) = term_lit {
                            if self.literal_at(at, lit) {
                                break;
                            }
                        }
                        let sub = self.parse_prod(*i)?;
                        kids.push(Child { width: sub.width, node: ChildNode::Sub(Arc::new(sub)) });
                    }
                }
            }
        }
        Ok(Tree { prod: p_idx, alt: ai, kids, width: (self.pos - start) as u32 })
    }

    /// Scan from `self.pos` to the next occurrence of `term` (after leading
    /// whitespace), or to end of input when `term` is `None`. The expression
    /// substring is everything in between (the embedded expr lexer skips its
    /// own leading/trailing whitespace).
    fn scan_to(&self, term: Option<&str>) -> usize {
        match term {
            None => self.b.len(),
            Some(lit) => {
                let k = lit.as_bytes();
                let mut i = self.pos;
                while i + k.len() <= self.b.len() {
                    if &self.b[i..i + k.len()] == k {
                        return i;
                    }
                    i += 1;
                }
                self.b.len()
            }
        }
    }
}

/// Fresh parse of a whole program (start production, must consume all input).
pub fn parse_program(src: &str) -> Result<Program, HostError> {
    let mut p = HostParser::new(src, 0);
    let root = p.parse_prod(GRAMMAR.start)?;
    if p.pos + p.ws_len() != src.len() {
        return Err(HostError::Trailing(p.pos as u32));
    }
    Ok(Program { root })
}

// ---- Incremental reparse: recursive reparseable-element reuse ----------

/// Reparse `old` after `edit`. Reuses every subtree outside the edited range
/// wholesale, recurses into the one subtree containing the edit, and reuses the
/// edited expression via the precedence-bounded predicate. Escalates to a fresh
/// reparse of the innermost enclosing production whose boundary is unchanged,
/// and finally to a full fresh parse. Always equals a fresh parse of the edited
/// source (the `host_incremental_matches_fresh` oracle).
pub fn reparse_program(
    old: &Program,
    old_src: &str,
    edit: &Edit,
) -> Result<(Program, String, HostStats), HostError> {
    let new_src = edit.apply(old_src);
    let mut stats = HostStats::default();
    let delta = edit.replacement.len() as i64 - (edit.end - edit.start) as i64;

    if let Some(root) = reparse_tree(&old.root, 0, edit, delta, old_src, &new_src, 0, &mut stats) {
        if root.width as usize == new_src.len() {
            return Ok((Program { root }, new_src, stats));
        }
    }

    // Sound fallback: full fresh parse.
    stats = HostStats { fell_back: true, ..HostStats::default() };
    let prog = parse_program(&new_src)?;
    stats.stmts_reparsed = prog.root.kids.len() as u32;
    Ok((prog, new_src, stats))
}

/// Returns `Some(new_subtree)` if this subtree can be soundly reused/reparsed
/// in place, or `None` to escalate to the caller. `abs_start` is the subtree's
/// absolute byte offset in `old_src`; `delta` is the global length change.
fn reparse_tree(
    old: &Tree,
    abs_start: u32,
    edit: &Edit,
    delta: i64,
    old_src: &str,
    new_src: &str,
    depth: u32,
    stats: &mut HostStats,
) -> Option<Tree> {
    // Locate the unique child whose span fully contains the edit.
    let mut pos = abs_start;
    let mut hit: Option<usize> = None;
    for (i, k) in old.kids.iter().enumerate() {
        let (cs, ce) = (pos, pos + k.width);
        if cs <= edit.start && edit.end <= ce {
            // Reject a pure boundary touch shared with the next child's start
            // (an insertion exactly at `ce` could belong to either side).
            if edit.start == ce && edit.end == ce && i + 1 < old.kids.len() {
                hit = None;
                break;
            }
            hit = Some(i);
            break;
        }
        pos += k.width;
    }
    let Some(ci) = hit else {
        // Edit spans/borders multiple children: reparse this production.
        return local_reparse(old, abs_start, delta, new_src, depth, stats);
    };

    let mut cpos = abs_start;
    for k in old.kids.iter().take(ci) {
        cpos += k.width;
    }
    let child = &old.kids[ci];
    let (cs, ce) = (cpos, cpos + child.width);

    let rebuilt: Option<Child> = match &child.node {
        ChildNode::Sub(sub) => {
            match reparse_tree(sub, cs, edit, delta, old_src, new_src, depth + 1, stats) {
                Some(t) => Some(Child { width: t.width, node: ChildNode::Sub(Arc::new(t)) }),
                None => return local_reparse(old, abs_start, delta, new_src, depth, stats),
            }
        }
        ChildNode::Expr(node) => {
            if !repl_is_expr_safe(&edit.replacement) {
                return local_reparse(old, abs_start, delta, new_src, depth, stats);
            }
            let local = Edit {
                start: edit.start - cs,
                end: edit.end - cs,
                replacement: edit.replacement.clone(),
            };
            match incremental_parse(node, &old_src[cs as usize..ce as usize], &local) {
                Ok((new_expr, _src, es)) => {
                    stats.exprs_reused += es.nodes_reused;
                    stats.reuse_depth = depth;
                    let w = (child.width as i64 + delta) as u32;
                    Some(Child { width: w, node: ChildNode::Expr(new_expr) })
                }
                Err(_) => return local_reparse(old, abs_start, delta, new_src, depth, stats),
            }
        }
        // Editing a literal or identifier changes structure: reparse this level.
        ChildNode::Tok(_) | ChildNode::Id(_) => {
            return local_reparse(old, abs_start, delta, new_src, depth, stats)
        }
    };

    let rebuilt = rebuilt?;
    // Rebuild: clone siblings (relative widths unchanged), splice the new child.
    let mut kids: Vec<Child> = Vec::with_capacity(old.kids.len());
    for (i, k) in old.kids.iter().enumerate() {
        if i == ci {
            kids.push(rebuilt.clone());
        } else {
            // Count only wholesale subtree reuse, not cloned literals.
            if matches!(k.node, ChildNode::Sub(_)) {
                stats.stmts_reused += 1;
            }
            kids.push(k.clone());
        }
    }
    let width = (old.width as i64 + delta) as u32;
    Some(Tree { prod: old.prod, alt: old.alt, kids, width })
}

/// Fresh-parse `old`'s production at `abs_start` over `new_src` and accept it
/// only if it consumes *exactly* its delta-adjusted span — i.e. its following
/// boundary token sits where the untouched suffix expects it. This is the
/// nearest-enclosing reparse (escalation step) and the one-token-lookahead
/// guard made exact.
fn local_reparse(
    old: &Tree,
    abs_start: u32,
    delta: i64,
    new_src: &str,
    depth: u32,
    stats: &mut HostStats,
) -> Option<Tree> {
    let expected_end = (abs_start as i64 + old.width as i64 + delta) as usize;
    let mut p = HostParser::new(new_src, abs_start as usize);
    let t = p.parse_prod(old.prod).ok()?;
    // Must consume exactly to the expected boundary (allowing trailing ws only
    // at the very top level, where the start production runs to EOF).
    let end = p.pos + if expected_end == new_src.len() { p.ws_len() } else { 0 };
    if end != expected_end {
        return None;
    }
    stats.stmts_reparsed += 1;
    stats.reuse_depth = depth;
    Some(t)
}

/// The expression-reuse guard: a replacement is safe to feed to the embedded
/// `incremental_parse` only if it introduces no host token bytes (punctuation
/// or keyword) that could change statement structure.
fn repl_is_expr_safe(repl: &str) -> bool {
    if repl.bytes().any(|b| GRAMMAR.punct_bytes.contains(&b)) {
        return false;
    }
    for kw in GRAMMAR.keywords {
        if repl.contains(kw) {
            return false;
        }
    }
    true
}
