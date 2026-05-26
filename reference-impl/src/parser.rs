//! Fresh Pratt parser producing Arc-shared trees with width-based positions.
//!
//! Implements `PrattCore` (see `pratt_core.rs`) with no reuse hook —
//! `try_reuse` defaults to `None`, collapsing the trait's `parse_expr`
//! body to fresh-parse behaviour. Comparator parsers (`incremental`,
//! `span_lookahead`, `roslyn_style`) share the same trait body and
//! override `try_reuse` with their respective predicates.

use std::sync::Arc;

use crate::ast::{Node, NodeKind};
use crate::lexer::{tokenize, Token, TokenKind};
use crate::op::MIN_PREC;
use crate::pratt_core::PrattCore;

pub use crate::pratt_core::PrattCore as _;

pub fn parse(src: &str) -> Result<Arc<Node>, ParseError> {
    let tokens = tokenize(src);
    let mut p = Parser {
        src,
        tokens: &tokens,
        pos: 0,
    };
    let node = p.parse_expr(MIN_PREC)?;
    if p.peek().kind != TokenKind::Eof {
        return Err(ParseError::TrailingTokens { at: p.peek().start });
    }
    Ok(node)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    UnexpectedToken { kind: TokenKind, at: u32 },
    UnexpectedEof,
    MissingRParen { at: u32 },
    MissingRBracket { at: u32 },
    MissingColon { at: u32 },
    MissingMemberName { at: u32 },
    TrailingTokens { at: u32 },
}

pub(crate) struct Parser<'a> {
    pub(crate) src: &'a str,
    pub(crate) tokens: &'a [Token],
    pub(crate) pos: usize,
}

impl<'a> PrattCore<'a> for Parser<'a> {
    fn src(&self) -> &'a str { self.src }
    fn tokens(&self) -> &'a [Token] { self.tokens }
    fn pos(&self) -> usize { self.pos }
    fn set_pos(&mut self, pos: usize) { self.pos = pos; }
    // try_reuse and on_parsed: use trait defaults (no reuse, noop).
}

/// Operand collected during associative-chain flattening, with the
/// absolute byte position where its first token began.
pub(crate) struct ChainOperand {
    pub start_byte: u32,
    pub node: Arc<Node>,
}

/// Build a balanced binary tree from a list of operands separated by
/// the same associative operator. `operands.len() >= 1`. Width of each
/// constructed Binary covers its operand range. `chain_lbp` is the
/// shared lbp of the operator (used as `m_spine`).
/// Operand-count of a chain subtree for the chain operator `op`: an
/// associativity-conflict spine node carries its count in `wb`; anything else
/// is a single operand (weight 1) from this chain's perspective.
#[cfg(feature = "chain_splice")]
pub(crate) fn chain_size(n: &Node, op: TokenKind) -> u32 {
    match &n.kind {
        NodeKind::Binary { op: o, .. } if *o == op => n.wb,
        _ => 1,
    }
}

pub(crate) fn build_balanced(
    op: TokenKind,
    operands: &[ChainOperand],
    chain_lbp: u32,
) -> Arc<Node> {
    if operands.len() == 1 {
        return Arc::clone(&operands[0].node);
    }
    // Ceil(n/2): for small N this matches the conventional left-leaning
    // shape (so `a+b+c` parses as `(a+b)+c`); for large N it still
    // produces a balanced O(log n) tree.
    let mid = (operands.len() + 1) / 2;
    let left = build_balanced(op, &operands[..mid], chain_lbp);
    let right = build_balanced(op, &operands[mid..], chain_lbp);
    let start = operands[0].start_byte;
    let last = operands.last().unwrap();
    let end = last.start_byte + last.node.width;
    #[cfg(feature = "chain_splice")]
    let wb = chain_size(&left, op) + chain_size(&right, op);
    Arc::new(Node {
        kind: NodeKind::Binary { op, left, right },
        width: end - start,
        m_spine: chain_lbp,
        // The rightmost operand's stop_lbp is what would have stopped a
        // parse_expr call ending at this balanced node's end. For
        // interior nodes of a chain that is *the same operator*, that
        // is `chain_lbp` (the chain continues). For the outermost
        // balanced node, it's whatever follows the chain in the source.
        stop_lbp: operands.last().unwrap().node.stop_lbp,
        m_floor: chain_lbp.saturating_sub(1),
        id: crate::ast::fresh_id(),
        #[cfg(feature = "chain_splice")]
        wb,
    })
}

// All parsing logic (`nud`, `led`, `parse_assoc_chain`, `parse_postfix`,
// `parse_expr`) is supplied by the `PrattCore` trait's default-method
// implementations — see `pratt_core.rs`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atom() {
        let n = parse("42").unwrap();
        assert_eq!(n.unparse(), "42");
        assert_eq!(n.m_spine, u32::MAX);
    }

    #[test]
    fn left_assoc() {
        let n = parse("a + b + c").unwrap();
        assert_eq!(n.unparse(), "((a + b) + c)");
        assert_eq!(n.m_spine, 50);
    }

    #[test]
    fn precedence() {
        let n = parse("a + b * c").unwrap();
        assert_eq!(n.unparse(), "(a + (b * c))");
        assert_eq!(n.m_spine, 50);
        if let NodeKind::Binary { right, .. } = &n.kind {
            assert_eq!(right.m_spine, 60);
        } else {
            panic!();
        }
    }

    #[test]
    fn right_assoc() {
        let n = parse("a ^ b ^ c").unwrap();
        assert_eq!(n.unparse(), "(a ^ (b ^ c))");
        assert_eq!(n.m_spine, 70);
    }

    #[test]
    fn prefix() {
        let n = parse("-a + b").unwrap();
        assert_eq!(n.unparse(), "((-a) + b)");
    }

    #[test]
    fn ternary() {
        let n = parse("a ? b : c ? d : e").unwrap();
        assert_eq!(n.unparse(), "(a ? b : (c ? d : e))");
    }

    #[test]
    fn parens() {
        let n = parse("(a + b) * c").unwrap();
        assert_eq!(n.unparse(), "((a + b) * c)");
    }

    #[test]
    fn call_no_args() {
        let n = parse("f()").unwrap();
        assert_eq!(n.unparse(), "(f())");
    }

    #[test]
    fn call_one_arg() {
        let n = parse("f(x)").unwrap();
        assert_eq!(n.unparse(), "(f(x))");
    }

    #[test]
    fn call_many_args() {
        let n = parse("f(a, b, c)").unwrap();
        assert_eq!(n.unparse(), "(f(a, b, c))");
    }

    #[test]
    fn call_arg_is_expr() {
        let n = parse("f(a + b * c)").unwrap();
        assert_eq!(n.unparse(), "(f((a + (b * c))))");
    }

    #[test]
    fn member_chain() {
        let n = parse("a.b.c").unwrap();
        assert_eq!(n.unparse(), "((a.b).c)");
    }

    #[test]
    fn index_basic() {
        let n = parse("a[i]").unwrap();
        assert_eq!(n.unparse(), "(a[i])");
    }

    #[test]
    fn postfix_mixed_chain() {
        let n = parse("a.b(x)[i].c").unwrap();
        assert_eq!(n.unparse(), "((((a.b)(x))[i]).c)");
    }

    #[test]
    fn postfix_binds_tighter_than_infix() {
        let n = parse("a + b.c").unwrap();
        assert_eq!(n.unparse(), "(a + (b.c))");
    }

    #[test]
    fn postfix_binds_tighter_than_prefix() {
        let n = parse("-a.b").unwrap();
        assert_eq!(n.unparse(), "(-(a.b))");
    }

    #[test]
    fn nested_calls() {
        let n = parse("f(g(x))").unwrap();
        assert_eq!(n.unparse(), "(f((g(x))))");
    }
}
