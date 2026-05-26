//! Fresh Pratt parser + balanced-tree builder. GENERATED-CRATE RUNTIME.
//!
//! Grammar-independent: all parsing logic lives in `PrattCore`
//! (`pratt_core.rs`), driven by the generated operator tables in `op.rs`.
//! The fresh `Parser` implements `PrattCore` with no reuse hook.

use std::sync::Arc;

use crate::ast::{Node, NodeKind};
use crate::cursor::Lexer;
use crate::lexer::TokenKind;
use crate::op::MIN_PREC;
use crate::pratt_core::PrattCore;

pub fn parse(src: &str) -> Result<Arc<Node>, ParseError> {
    let mut p = Parser { src, lexer: Lexer::new(src) };
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
    MissingCloseParen { at: u32 },
    TrailingTokens { at: u32 },
}

pub(crate) struct Parser<'a> {
    pub(crate) src: &'a str,
    pub(crate) lexer: Lexer<'a>,
}

impl<'a> PrattCore<'a> for Parser<'a> {
    fn src(&self) -> &'a str { self.src }
    fn lexer(&self) -> &Lexer<'a> { &self.lexer }
    fn lexer_mut(&mut self) -> &mut Lexer<'a> { &mut self.lexer }
}

/// Operand collected during associativity-conflict chain flattening,
/// tagged with the absolute byte where its first token began.
pub(crate) struct ChainOperand {
    pub start_byte: u32,
    pub node: Arc<Node>,
}

/// Build a balanced binary tree from operands separated by the same
/// associativity-conflict operator. `operands.len() >= 1`. `chain_lbp`
/// is the operator's lbp, used as `m_spine` on constructed nodes.
///
/// Interior nodes inherit `stop_lbp` from the rightmost operand of their
/// slice — NOT 0. (Setting interior `stop_lbp = 0` was a real soundness
/// bug: it let an interior accumulator be reused at a floor where the new
/// parser would continue absorbing past its end. Only the outermost node
/// carries the chain's true exit `stop_lbp`.)
pub(crate) fn build_balanced(op: TokenKind, operands: &[ChainOperand], chain_lbp: u32) -> Arc<Node> {
    if operands.len() == 1 {
        return Arc::clone(&operands[0].node);
    }
    // Ceil(n/2): matches conventional left-leaning shape for small N
    // (`a+b+c` => `(a+b)+c`), stays balanced (O(log n)) for large N.
    let mid = (operands.len() + 1) / 2;
    let left = build_balanced(op, &operands[..mid], chain_lbp);
    let right = build_balanced(op, &operands[mid..], chain_lbp);
    let start = operands[0].start_byte;
    let last = operands.last().unwrap();
    let end = last.start_byte + last.node.width;
    Arc::new(Node {
        kind: NodeKind::Binary { op, left, right },
        width: end - start,
        m_spine: chain_lbp,
        stop_lbp: last.node.stop_lbp,
        m_floor: chain_lbp.saturating_sub(1),
    })
}
