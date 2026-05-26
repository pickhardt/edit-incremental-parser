//! Operator binding-power tables.
//!
//! Precedence values are integers; higher = tighter. For right-associative
//! infix operators, `rbp = lbp - 1` (standard Pratt trick). For left-assoc
//! infix operators, `rbp = lbp`.
//!
//! Sentinel: `MIN_PREC = 0` is the floor passed to the top-level call,
//! and is below any real operator's lbp. `LBP_NONE = 0` is used for
//! non-operator tokens (atoms, EOF, closers) so the parser loop exits.

use crate::lexer::TokenKind;

pub const MIN_PREC: u32 = 0;
pub const LBP_NONE: u32 = 0;

/// Left-binding-power: how tightly this token binds to its left.
/// Returns 0 for tokens that cannot appear in infix or postfix position.
pub fn lbp(kind: TokenKind) -> u32 {
    use TokenKind::*;
    match kind {
        Question => 5,    // ternary `? :` — right assoc
        OrOr => 10,
        AndAnd => 20,
        EqEq | BangEq => 30,
        Lt | Gt | LtEq | GtEq => 40,
        Plus | Minus => 50,
        Star | Slash | Percent => 60,
        Caret => 70, // right assoc (exponentiation)
        // Postfix operators bind tightest: function call `f(args)`,
        // indexing `a[i]`, and member access `a.b`. The lbp value
        // governs absorption order (so `a.b()` parses as `(a.b)()`,
        // and `-a.b` parses as `-(a.b)` because prefix `-` has
        // rbp=80 > lbp(Dot)=90 — wait no, postfix should be tighter
        // than prefix. Set postfix to 90 > prefix_rbp(80).
        LParen | LBracket | Dot => 90,
        _ => LBP_NONE,
    }
}

/// True iff `kind` is a postfix operator: function call `(`, indexing
/// `[`, or member access `.`. Postfix operators consume specific
/// closing-token sequences rather than recursing into another
/// `parse_expr(rbp)` for the right operand — they are dispatched
/// separately from `led()` in the parser loop.
pub fn is_postfix(kind: TokenKind) -> bool {
    matches!(kind, TokenKind::LParen | TokenKind::LBracket | TokenKind::Dot)
}

/// Right-binding-power: the `min_prec` passed to the recursive call inside
/// `led`. Equal to `lbp` for left-assoc, `lbp - 1` for right-assoc.
pub fn rbp(kind: TokenKind) -> u32 {
    use TokenKind::*;
    match kind {
        // Right-associative
        Caret => lbp(kind) - 1,
        Question => lbp(kind) - 1,
        // Left-associative
        _ => lbp(kind),
    }
}

/// True iff `kind` can start an infix `led` call.
pub fn is_infix(kind: TokenKind) -> bool {
    lbp(kind) > LBP_NONE
}

/// True iff `kind` can start a `nud` (prefix-position) parse.
pub fn is_prefix(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Minus | TokenKind::Bang | TokenKind::LParen | TokenKind::Int | TokenKind::Ident
    )
}

/// Right-binding-power for a prefix operator's nud call.
/// Prefix operators bind tighter than any infix: we use a value above
/// the highest infix lbp.
pub fn prefix_rbp(kind: TokenKind) -> u32 {
    match kind {
        TokenKind::Minus | TokenKind::Bang => 80,
        _ => 0,
    }
}

/// Classification of binary operators by precedence-conflict type,
/// following Li and Taura's AOPP typology (HPC Asia 2023, §3.3).
///
/// Every precedence conflict between two same-precedence terminals is
/// exactly one of:
///   * **Strong** — unresolvable by any associativity choice. AOPP §3.1
///     Definition 3.1. Examples in real languages: dangling-else,
///     generic-vs-comparison `<`, automatic semicolon insertion. Such
///     constructs require escalation (full reparse of enclosing scope)
///     and admit no precedence-bounded reuse.
///   * **Weak** (`T ≷̲ T`) — resolvable by declaring left- or right-
///     associativity. AOPP §3.3 Definition 3.3. Standard Pratt
///     left-leaning or right-leaning trees, identical to OPP behavior.
///   * **AssociativityConflict** (`T ≷̅ T`) — both directions safe;
///     fold order is semantically irrelevant. AOPP §3.3 Definition 3.5.
///     AOPP exploits this for parallel any-order reduction; we exploit
///     it for balanced-tree representation and edit-local reuse.
///
/// Our toy grammar contains no strong-conflict constructs; the variant
/// is included for completeness and to make the typology explicit at
/// the type level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorClass {
    /// Unresolvable by associativity choice; cannot reuse locally.
    Strong,
    /// Resolvable by declared associativity; standard Pratt fold.
    Weak,
    /// Both directions safe; admits balanced-tree representation.
    AssociativityConflict,
}

/// Classify a token's precedence-conflict type per Li & Taura's AOPP
/// typology. Non-operator tokens classify as `Strong` (cannot participate
/// in any reuse decision); the parser only dispatches on this for tokens
/// it has already decided are operators (i.e. `lbp > min_prec`).
pub fn operator_class(kind: TokenKind) -> OperatorClass {
    use TokenKind::*;
    match kind {
        // Associativity conflicts: mathematically associative operators.
        // `(a + b) + c` and `a + (b + c)` denote the same value; the tree
        // shape is semantically under-determined and can be balanced
        // freely for edit-local reuse.
        Plus | Star | AndAnd | OrOr => OperatorClass::AssociativityConflict,

        // Weak conflicts: declared associativity disambiguates a parse-
        // tree shape that is NOT semantically under-determined.
        //   * `-`, `/`, `%`: `(a-b)-c` ≠ `a-(b-c)` semantically.
        //   * `==`, `!=`, `<`, `>`, `<=`, `>=`: not chainable; `(a<b)<c`
        //     is `bool < int`, nonsense.
        //   * `^`: declared right-associative.
        //   * `?` (ternary): three-way structure with fixed assoc.
        Minus | Slash | Percent | EqEq | BangEq | Lt | Gt | LtEq | GtEq | Caret | Question => {
            OperatorClass::Weak
        }

        // Postfix operators (function call, indexing, member access):
        // left-associative chains (`a.b.c` = `(a.b).c`, `a()()` =
        // `(a())()`); no balancing applies because they are not
        // mathematically commutative or associative in any way.
        LParen | LBracket | Dot => OperatorClass::Weak,

        // All other tokens classify as Strong (which for non-operators
        // means "no participation in reuse decisions"). Real grammars
        // with constructs like dangling-else would also classify those
        // as Strong here.
        _ => OperatorClass::Strong,
    }
}

/// Convenience: AssociativityConflict ops are those that may be folded
/// into a balanced (rather than strictly left- or right-leaning) tree.
/// Equivalent to `operator_class(kind) == OperatorClass::AssociativityConflict`.
pub fn is_associativity_conflict(kind: TokenKind) -> bool {
    matches!(operator_class(kind), OperatorClass::AssociativityConflict)
}

/// Back-compat alias for the old name. Prefer `is_associativity_conflict`.
pub fn is_associative(kind: TokenKind) -> bool {
    is_associativity_conflict(kind)
}
