//! Shared Pratt machinery. GENERATED-CRATE RUNTIME (grammar-independent).
//!
//! `nud`, `led`, `parse_assoc_chain`, `parse_postfix_unary`, and the
//! `parse_expr` loop are all driven by the generated operator predicates
//! in `op.rs` — no token kind is hardcoded here. The fresh `Parser` and
//! the `IncrementalParser` both implement this trait; the only difference
//! is `try_reuse` (default `None` for fresh; the precedence-bounded
//! predicate for incremental).

use std::sync::Arc;

use crate::ast::{Node, NodeKind};
use crate::cursor::Lexer;
use crate::lexer::{Token, TokenKind};
use crate::op::{
    close_paren_kind, is_atom, is_open_paren, is_postfix_op, is_prefix_op, lbp, operator_class,
    prefix_rbp, rbp, OperatorClass, MIN_PREC,
};
use crate::parser::{build_balanced, ChainOperand, ParseError};

pub trait PrattCore<'a> {
    // --- Required accessors ---
    fn src(&self) -> &'a str;
    fn lexer(&self) -> &Lexer<'a>;
    fn lexer_mut(&mut self) -> &mut Lexer<'a>;

    // --- Optional hooks ---
    /// Reuse hook. Default: no reuse (fresh-parse behaviour). The
    /// incremental parser overrides this with the precedence-bounded
    /// predicate.
    fn try_reuse(&mut self, _min_prec: u32) -> Option<Arc<Node>> {
        None
    }
    /// Called after a fresh node is constructed (stats hook). Default noop.
    fn on_parsed(&mut self) {}

    // --- Trivial methods (tokens pulled on demand from the lexer) ---
    fn peek(&self) -> Token {
        self.lexer().peek()
    }
    fn advance(&mut self) -> Token {
        self.lexer_mut().advance()
    }
    fn last_end(&self) -> u32 {
        self.lexer().last_end()
    }

    // --- Core parse loop ---
    fn parse_expr(&mut self, min_prec: u32) -> Result<Arc<Node>, ParseError> {
        let start_byte = self.peek().start;
        let mut left = match self.try_reuse(min_prec) {
            Some(n) => n,
            None => self.nud(start_byte)?,
        };
        Arc::make_mut(&mut left).stop_lbp = lbp(self.peek().kind);

        let mut spine_min = u32::MAX;
        loop {
            let next_kind = self.peek().kind;
            let next_lbp = lbp(next_kind);
            if next_lbp <= min_prec {
                break;
            }
            spine_min = spine_min.min(next_lbp);

            if is_postfix_op(next_kind) {
                left = self.parse_postfix_unary(left, start_byte, min_prec, spine_min)?;
            } else {
                // Three-way dispatch on the AOPP conflict typology.
                match operator_class(next_kind) {
                    OperatorClass::AssociativityConflict => {
                        left = self.parse_assoc_chain(left, next_kind, start_byte)?;
                    }
                    OperatorClass::Weak | OperatorClass::Strong => {
                        left = self.led(left, start_byte, min_prec, spine_min)?;
                    }
                }
            }
            Arc::make_mut(&mut left).stop_lbp = lbp(self.peek().kind);
        }
        if spine_min < u32::MAX {
            let m = Arc::make_mut(&mut left);
            m.m_spine = spine_min;
            m.m_floor = min_prec;
        }
        Ok(left)
    }

    fn nud(&mut self, start_byte: u32) -> Result<Arc<Node>, ParseError> {
        let t = self.peek();
        if is_atom(t.kind) {
            self.on_parsed();
            self.advance();
            return Ok(Arc::new(Node::atom(t.text(self.src()), self.last_end() - start_byte)));
        }
        if is_prefix_op(t.kind) {
            self.on_parsed();
            self.advance();
            let child_min_prec = prefix_rbp(t.kind);
            let child = self.parse_expr(child_min_prec)?;
            return Ok(Arc::new(Node {
                kind: NodeKind::Prefix { op: t.kind, child },
                width: self.last_end() - start_byte,
                m_spine: u32::MAX,
                stop_lbp: 0,
                m_floor: child_min_prec,
            }));
        }
        if is_open_paren(t.kind) {
            self.on_parsed();
            self.advance();
            let inner = self.parse_expr(MIN_PREC)?;
            let close = self.peek();
            if close.kind != close_paren_kind() {
                return Err(ParseError::MissingCloseParen { at: close.start });
            }
            self.advance();
            return Ok(Arc::new(Node {
                kind: NodeKind::Paren { inner },
                width: self.last_end() - start_byte,
                m_spine: u32::MAX,
                stop_lbp: 0,
                m_floor: 0,
            }));
        }
        match t.kind {
            TokenKind::Eof => Err(ParseError::UnexpectedEof),
            _ => Err(ParseError::UnexpectedToken { kind: t.kind, at: t.start }),
        }
    }

    fn led(
        &mut self,
        left: Arc<Node>,
        outer_start: u32,
        outer_min_prec: u32,
        outer_spine_min: u32,
    ) -> Result<Arc<Node>, ParseError> {
        let op_kind = self.peek().kind;
        self.on_parsed();
        self.advance();
        let right = self.parse_expr(rbp(op_kind))?;
        Ok(Arc::new(Node {
            kind: NodeKind::Binary { op: op_kind, left, right },
            width: self.last_end() - outer_start,
            m_spine: outer_spine_min,
            stop_lbp: 0,
            m_floor: outer_min_prec,
        }))
    }

    fn parse_postfix_unary(
        &mut self,
        left: Arc<Node>,
        outer_start: u32,
        outer_min_prec: u32,
        outer_spine_min: u32,
    ) -> Result<Arc<Node>, ParseError> {
        let op_kind = self.peek().kind;
        self.on_parsed();
        self.advance();
        Ok(Arc::new(Node {
            kind: NodeKind::Postfix { op: op_kind, child: left },
            width: self.last_end() - outer_start,
            m_spine: outer_spine_min,
            stop_lbp: 0,
            m_floor: outer_min_prec,
        }))
    }

    fn parse_assoc_chain(
        &mut self,
        left: Arc<Node>,
        chain_op: TokenKind,
        outer_start: u32,
    ) -> Result<Arc<Node>, ParseError> {
        let chain_lbp = lbp(chain_op);
        let mut operands: Vec<ChainOperand> =
            vec![ChainOperand { start_byte: outer_start, node: left }];
        self.advance();
        let operand_start = self.peek().start;
        let right = self.parse_expr(rbp(chain_op))?;
        operands.push(ChainOperand { start_byte: operand_start, node: right });
        while self.peek().kind == chain_op {
            self.advance();
            let operand_start = self.peek().start;
            let right = self.parse_expr(rbp(chain_op))?;
            operands.push(ChainOperand { start_byte: operand_start, node: right });
        }
        Ok(build_balanced(chain_op, &operands, chain_lbp))
    }
}
