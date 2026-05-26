//! Arc-shared AST with width-based positions. GENERATED-CRATE RUNTIME.
//!
//! This file is grammar-independent and is copied verbatim into every
//! generated parser crate. Roslyn's red/green tree idea adapted to Pratt:
//! each `Node` stores its byte length (`width`) instead of an absolute
//! span, so a cached subtree reused at a new position needs no
//! translation. Children are `Arc<Node>`; reusing a cached subtree is one
//! `Arc::clone` — a refcount bump, O(1) regardless of subtree size.
//!
//! Reuse metadata (the precedence-bounded reuse predicate, see
//! `incremental.rs`): every node carries
//!   * `m_spine`  — minimum lbp of operators absorbed at the top loop of
//!                  the producing `parse_expr` call. `u32::MAX` if none
//!                  (atom / prefix-only / parenthesised). Upper bound of
//!                  the acceptance band.
//!   * `stop_lbp` — lbp of the token that immediately followed this
//!                  subtree in the source it was parsed from. Lower bound
//!                  of the acceptance band.
//!   * `m_floor`  — `min_prec` the producing call ran under (diagnostics).

use std::sync::Arc;

use crate::lexer::TokenKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub width: u32,
    pub kind: NodeKind,
    pub m_spine: u32,
    pub stop_lbp: u32,
    pub m_floor: u32,
}

/// The fixed five-variant node shape supported by the generator's
/// operator-expression fragment: atoms, prefix-unary, postfix-unary,
/// infix-binary, and parenthesised groups. Ternary / call / index /
/// member are out of scope for this proof-of-concept (see README).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Atom(Arc<str>),
    Prefix { op: TokenKind, child: Arc<Node> },
    Postfix { op: TokenKind, child: Arc<Node> },
    Binary { op: TokenKind, left: Arc<Node>, right: Arc<Node> },
    Paren { inner: Arc<Node> },
}

impl Node {
    pub fn atom(text: &str, width: u32) -> Self {
        Node {
            kind: NodeKind::Atom(Arc::from(text)),
            width,
            m_spine: u32::MAX,
            stop_lbp: 0,
            m_floor: 0,
        }
    }

    pub fn count(&self) -> u32 {
        let kids: u32 = match &self.kind {
            NodeKind::Atom(_) => 0,
            NodeKind::Prefix { child, .. } => child.count(),
            NodeKind::Postfix { child, .. } => child.count(),
            NodeKind::Binary { left, right, .. } => left.count() + right.count(),
            NodeKind::Paren { inner } => inner.count(),
        };
        1 + kids
    }

    /// Canonical structural representation. Used by tests to compare
    /// parses without depending on byte positions.
    pub fn unparse(&self) -> String {
        match &self.kind {
            NodeKind::Atom(s) => s.to_string(),
            NodeKind::Prefix { op, child } => {
                format!("({}{})", crate::grammar_text::token_text(*op), child.unparse())
            }
            NodeKind::Postfix { op, child } => {
                format!("({}{})", child.unparse(), crate::grammar_text::token_text(*op))
            }
            NodeKind::Binary { op, left, right } => format!(
                "({} {} {})",
                left.unparse(),
                crate::grammar_text::token_text(*op),
                right.unparse()
            ),
            NodeKind::Paren { inner } => inner.unparse(),
        }
    }

    /// Canonical representation that **normalises associativity-conflict
    /// regions**: chains of the same associativity-conflict operator
    /// (e.g. `+`, `*`) flatten to a single n-ary form, so trees that
    /// differ only by re-association compare equal. This is the
    /// semantic-equivalence comparison (Li & Taura "relaxed syntax, firm
    /// semantics") used by the oracle to avoid counting a balanced-tree
    /// re-association as a soundness failure.
    pub fn unparse_normalized(&self) -> String {
        use crate::op::is_associativity_conflict;
        match &self.kind {
            NodeKind::Atom(s) => s.to_string(),
            NodeKind::Prefix { op, child } => format!(
                "({}{})",
                crate::grammar_text::token_text(*op),
                child.unparse_normalized()
            ),
            NodeKind::Postfix { op, child } => format!(
                "({}{})",
                child.unparse_normalized(),
                crate::grammar_text::token_text(*op)
            ),
            NodeKind::Binary { op, .. } if is_associativity_conflict(*op) => {
                let mut operands = Vec::new();
                flatten_assoc(self, *op, &mut operands);
                let op_text = crate::grammar_text::token_text(*op);
                let strs: Vec<String> = operands.iter().map(|o| o.unparse_normalized()).collect();
                format!("({})", strs.join(&format!(" {} ", op_text)))
            }
            NodeKind::Binary { op, left, right } => format!(
                "({} {} {})",
                left.unparse_normalized(),
                crate::grammar_text::token_text(*op),
                right.unparse_normalized()
            ),
            NodeKind::Paren { inner } => inner.unparse_normalized(),
        }
    }
}

/// Flatten a chain of the same associativity-conflict operator into a
/// list of operands. Used by `unparse_normalized`.
fn flatten_assoc<'a>(node: &'a Node, chain_op: TokenKind, out: &mut Vec<&'a Node>) {
    match &node.kind {
        NodeKind::Binary { op, left, right } if *op == chain_op => {
            flatten_assoc(left, chain_op, out);
            flatten_assoc(right, chain_op, out);
        }
        _ => out.push(node),
    }
}
