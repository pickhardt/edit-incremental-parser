//! Arc-shared AST with width-based positions.
//!
//! Roslyn's red/green tree idea adapted to Pratt: each `Node` stores its
//! byte length (`width`) instead of absolute byte positions. Children
//! are `Arc<Node>`; sharing a cached subtree across two parses (or two
//! positions in the same parse) is one `Arc::clone` — a refcount bump.
//!
//! Reuse metadata: every `Node` carries
//!   * `m_spine` — minimum lbp of operators absorbed at the top loop of
//!     the producing `parse_expr` call. `u32::MAX` if no operators were
//!     absorbed (atom, prefix-only, or parenthesised group). The upper
//!     bound of the precedence-bounded acceptance band.
//!   * `stop_lbp` — lbp of the token that immediately followed this
//!     subtree in the source it was parsed from. The token that stopped
//!     the producing `parse_expr` loop. The lower bound of both the
//!     precedence-bounded and Roslyn-style acceptance bands.
//!   * `m_floor` — `min_prec` passed into the producing `parse_expr`.
//!     Used by the Roslyn-style comparator (`roslyn_style.rs`) as its
//!     acceptance upper bound (`M_new <= cand.m_floor`); this band is
//!     strictly tighter than the precedence-bounded band `[stop_lbp,
//!     m_spine)` because `M_floor < m_spine` whenever any operator was
//!     absorbed. Not used by the precedence-bounded predicate itself.
//!
//! **Two-sided precedence-bounded reuse predicate** (see `incremental.rs`):
//!   * `node.stop_lbp <= M_new < node.m_spine`
//!   * tokens within `node` are unchanged
//!   * bytes immediately before and after `node` are unchanged
//!     (tokenization-boundary check)

use std::sync::Arc;

use crate::lexer::TokenKind;

/// Stable per-node identity used by identity-keyed incremental semantics
/// (see `semantics.rs` and `identity_keyed_semantics_plan.md`). A fresh id is
/// minted at node construction and **preserved across reuse**: the incremental
/// parser reuses a subtree via `Arc::clone`, and even though the Pratt core
/// `Arc::make_mut`s the reused root to stamp `stop_lbp`, `Clone` copies the
/// `id` field, so a reused subtree keeps its identity. A fresh parse mints new
/// ids. This is the signal a memoizing semantic layer keys on.
///
/// Minting is gated behind the `node_id` feature so the default build (and the
/// §5 benchmarks) pays no counter cost on the hot parse path: with the feature
/// off, `fresh_id()` is the constant `NodeId(0)` and the only effect is the
/// field's size. `PartialEq`/`Eq` for `Node` deliberately ignore `id` so that
/// structural tree comparisons (e.g. `assert_eq!(tree, fresh)`) are unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub u64);

#[cfg(feature = "node_id")]
thread_local! {
    static NODE_COUNTER: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

/// Mint a fresh node id. Monotonic (never reused), so a stale memo entry can
/// never be a false hit — no eviction is required for correctness. Zero-cost
/// (`NodeId(0)`) unless the `node_id` feature is enabled.
#[inline(always)]
pub fn fresh_id() -> NodeId {
    #[cfg(feature = "node_id")]
    {
        NODE_COUNTER.with(|c| {
            let v = c.get();
            c.set(v + 1);
            NodeId(v)
        })
    }
    #[cfg(not(feature = "node_id"))]
    {
        NodeId(0)
    }
}

#[derive(Debug, Clone)]
pub struct Node {
    pub width: u32,
    pub kind: NodeKind,
    pub m_spine: u32,
    pub stop_lbp: u32,
    pub m_floor: u32,
    /// Stable identity; minted at construction, preserved across reuse.
    /// Ignored by `PartialEq`/`Eq` (see [`NodeId`]).
    pub id: NodeId,
    /// Subtree **operand count** for the weight-balanced chain splice
    /// (`chain_wb.rs`): for an associativity-conflict chain-spine node it is the
    /// number of chain operands beneath it; for everything else it is 1 (a leaf
    /// from any chain's perspective). Lets the weight-balanced `join` read
    /// subtree sizes in O(1). Gated; ignored by `PartialEq`/`Eq`.
    #[cfg(feature = "chain_splice")]
    pub wb: u32,
}

/// Structural equality **ignoring `id`**: two nodes are equal iff they have
/// the same shape and reuse metadata, regardless of identity. Keeps
/// `assert_eq!(tree, fresh)`-style structural comparisons working when ids
/// differ between an incremental and a fresh parse.
impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && self.m_spine == other.m_spine
            && self.stop_lbp == other.stop_lbp
            && self.m_floor == other.m_floor
            && self.kind == other.kind
    }
}
impl Eq for Node {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Atom(Arc<str>),
    Prefix {
        op: TokenKind,
        child: Arc<Node>,
    },
    Binary {
        op: TokenKind,
        left: Arc<Node>,
        right: Arc<Node>,
    },
    Ternary {
        cond: Arc<Node>,
        then: Arc<Node>,
        else_: Arc<Node>,
    },
    Paren {
        inner: Arc<Node>,
    },
    /// Postfix function call `callee(args)`. `args` may be empty (`f()`).
    Call {
        callee: Arc<Node>,
        args: Vec<Arc<Node>>,
    },
    /// Postfix indexing `array[index]`. Single index expression only.
    Index {
        array: Arc<Node>,
        index: Arc<Node>,
    },
    /// Postfix member access `object.field`. Field is an identifier.
    Member {
        object: Arc<Node>,
        field: Arc<str>,
    },
    /// Recovery: a zero-width synthesized operand or identifier, produced
    /// by an `Insert` repair (e.g. the right operand of `1 +`). Never
    /// appears in a clean parse. Carries sentinel reuse metadata and is
    /// never cached for reuse.
    Missing,
    /// Recovery: tokens that were deleted / could not be interpreted at
    /// this position, recorded for diagnostics. Produced by `Delete`
    /// repairs and the cost-bound fallback. Never appears in a clean
    /// parse; never cached.
    Error {
        skipped: Vec<TokenKind>,
    },
}

impl Node {
    pub fn atom(text: &str, width: u32) -> Self {
        Node {
            kind: NodeKind::Atom(Arc::from(text)),
            width,
            m_spine: u32::MAX,
            stop_lbp: 0,
            m_floor: 0,
            id: fresh_id(),
            #[cfg(feature = "chain_splice")]
            wb: 1,
        }
    }

    /// Canonical structural representation. Used in tests to compare
    /// parses across implementations without depending on byte positions.
    pub fn unparse(&self) -> String {
        match &self.kind {
            NodeKind::Atom(s) => s.to_string(),
            NodeKind::Prefix { op, child } => format!("({}{})", token_text(*op), child.unparse()),
            NodeKind::Binary { op, left, right } => {
                format!("({} {} {})", left.unparse(), token_text(*op), right.unparse())
            }
            NodeKind::Ternary { cond, then, else_ } => format!(
                "({} ? {} : {})",
                cond.unparse(),
                then.unparse(),
                else_.unparse()
            ),
            NodeKind::Paren { inner } => inner.unparse(),
            NodeKind::Call { callee, args } => {
                let args_str: Vec<String> = args.iter().map(|a| a.unparse()).collect();
                format!("({}({}))", callee.unparse(), args_str.join(", "))
            }
            NodeKind::Index { array, index } => {
                format!("({}[{}])", array.unparse(), index.unparse())
            }
            NodeKind::Member { object, field } => {
                format!("({}.{})", object.unparse(), field)
            }
            NodeKind::Missing => "⟨missing⟩".to_string(),
            NodeKind::Error { .. } => "⟨error⟩".to_string(),
        }
    }

    pub fn count(&self) -> u32 {
        let kids: u32 = match &self.kind {
            NodeKind::Atom(_) => 0,
            NodeKind::Prefix { child, .. } => child.count(),
            NodeKind::Binary { left, right, .. } => left.count() + right.count(),
            NodeKind::Ternary { cond, then, else_ } => {
                cond.count() + then.count() + else_.count()
            }
            NodeKind::Paren { inner } => inner.count(),
            NodeKind::Call { callee, args } => {
                callee.count() + args.iter().map(|a| a.count()).sum::<u32>()
            }
            NodeKind::Index { array, index } => array.count() + index.count(),
            NodeKind::Member { object, .. } => object.count(),
            NodeKind::Missing => 0,
            NodeKind::Error { .. } => 0,
        };
        1 + kids
    }

    /// Canonical structural representation that **normalizes
    /// associativity-conflict regions**. Two trees that differ only by
    /// re-grouping inside a chain of the same associative operator
    /// (`+`, `*`, `&&`, `||`) produce identical normalized output.
    ///
    /// This is the comparison the bench uses for the "incorrect" check:
    /// it distinguishes real semantic divergence (different value when
    /// evaluated) from associative-grouping divergence (semantically
    /// equivalent under associativity). The latter is by design for
    /// AssociativityConflict operators — Li and Taura AOPP §3.2 calls
    /// it "relaxed syntax, firm semantics" — and should not be counted
    /// as a soundness failure.
    pub fn unparse_normalized(&self) -> String {
        use crate::op::is_associativity_conflict;
        match &self.kind {
            NodeKind::Atom(s) => s.to_string(),
            NodeKind::Prefix { op, child } => {
                format!("({}{})", token_text(*op), child.unparse_normalized())
            }
            NodeKind::Binary { op, .. } if is_associativity_conflict(*op) => {
                let mut operands = Vec::new();
                flatten_assoc(self, *op, &mut operands);
                let op_text = token_text(*op);
                let strs: Vec<String> =
                    operands.iter().map(|o| o.unparse_normalized()).collect();
                format!("({})", strs.join(&format!(" {} ", op_text)))
            }
            NodeKind::Binary { op, left, right } => {
                format!(
                    "({} {} {})",
                    left.unparse_normalized(),
                    token_text(*op),
                    right.unparse_normalized()
                )
            }
            NodeKind::Ternary { cond, then, else_ } => format!(
                "({} ? {} : {})",
                cond.unparse_normalized(),
                then.unparse_normalized(),
                else_.unparse_normalized()
            ),
            NodeKind::Paren { inner } => inner.unparse_normalized(),
            NodeKind::Call { callee, args } => {
                let args_str: Vec<String> =
                    args.iter().map(|a| a.unparse_normalized()).collect();
                format!("({}({}))", callee.unparse_normalized(), args_str.join(", "))
            }
            NodeKind::Index { array, index } => format!(
                "({}[{}])",
                array.unparse_normalized(),
                index.unparse_normalized()
            ),
            NodeKind::Member { object, field } => {
                format!("({}.{})", object.unparse_normalized(), field)
            }
            NodeKind::Missing => "⟨missing⟩".to_string(),
            NodeKind::Error { .. } => "⟨error⟩".to_string(),
        }
    }

    /// Zero-width synthesized operand/identifier (an `Insert` repair).
    pub fn missing() -> Self {
        Node {
            kind: NodeKind::Missing,
            width: 0,
            m_spine: u32::MAX,
            stop_lbp: 0,
            m_floor: 0,
            id: fresh_id(),
            #[cfg(feature = "chain_splice")]
            wb: 1,
        }
    }

    /// Marker for deleted / uninterpretable tokens at this position.
    pub fn error(skipped: Vec<TokenKind>, width: u32) -> Self {
        Node {
            kind: NodeKind::Error { skipped },
            width,
            m_spine: u32::MAX,
            stop_lbp: 0,
            m_floor: 0,
            id: fresh_id(),
            #[cfg(feature = "chain_splice")]
            wb: 1,
        }
    }
}

/// Walk a subtree, flattening chains of the same associative operator
/// into a list of operands. Used by `unparse_normalized`.
fn flatten_assoc<'a>(
    node: &'a Node,
    chain_op: TokenKind,
    out: &mut Vec<&'a Node>,
) {
    match &node.kind {
        NodeKind::Binary { op, left, right } if *op == chain_op => {
            flatten_assoc(left, chain_op, out);
            flatten_assoc(right, chain_op, out);
        }
        _ => out.push(node),
    }
}

pub fn token_text(t: TokenKind) -> &'static str {
    use TokenKind::*;
    match t {
        Plus => "+",
        Minus => "-",
        Star => "*",
        Slash => "/",
        Percent => "%",
        Caret => "^",
        Bang => "!",
        AndAnd => "&&",
        OrOr => "||",
        EqEq => "==",
        BangEq => "!=",
        Lt => "<",
        Gt => ">",
        LtEq => "<=",
        GtEq => ">=",
        Question => "?",
        Colon => ":",
        LParen => "(",
        RParen => ")",
        LBracket => "[",
        RBracket => "]",
        Dot => ".",
        Comma => ",",
        Int => "<int>",
        Ident => "<id>",
        Eof => "<eof>",
    }
}
