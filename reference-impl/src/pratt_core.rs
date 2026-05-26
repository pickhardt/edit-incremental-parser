//! Shared Pratt-parsing machinery — `nud`, `led`, `parse_assoc_chain`,
//! `parse_postfix`, `parse_expr` — factored out of the four parser
//! implementations (`parser`, `incremental`, `span_lookahead`,
//! `roslyn_style`) so each parser only carries its own state and
//! overrides `try_reuse`.
//!
//! Design: trait with default-method implementations. Each parser
//! implements `PrattCore` by providing accessors for its mutable
//! state (`src`, `tokens`, `pos`) and optionally overrides
//! `try_reuse` (default: no reuse — the fresh-parse behaviour) and
//! `on_parsed` (default: noop — used by the incremental parsers to
//! track `nodes_parsed` statistics).
//!
//! Correctness: the trait's default `parse_expr` mirrors the
//! `Parser::parse_expr` body documented in `parser.rs`. The reuse
//! hook is consulted at the top of each recursion frame; for the
//! fresh `Parser` it always returns `None`, so the body collapses
//! to the original fresh-parse logic.

use std::sync::Arc;

use crate::ast::{Node, NodeKind};
use crate::lexer::{Token, TokenKind};
use crate::op::{
    is_postfix, is_prefix, lbp, operator_class, prefix_rbp, rbp, OperatorClass, MIN_PREC,
};
use crate::parser::{build_balanced, ChainOperand, ParseError};

pub trait PrattCore<'a> {
    // --- Required accessors ---
    fn src(&self) -> &'a str;
    fn tokens(&self) -> &'a [Token];
    fn pos(&self) -> usize;
    fn set_pos(&mut self, pos: usize);

    // --- Optional hooks (default impls) ---
    /// Reuse hook. Default: no reuse (fresh-parse behaviour).
    /// Comparator parsers (`incremental`, `span_lookahead`,
    /// `roslyn_style`) override this with their predicate.
    fn try_reuse(&mut self, _min_prec: u32) -> Option<Arc<Node>> {
        None
    }
    /// Called after a fresh-parsed node is constructed. Used by
    /// comparator parsers to increment `stats.nodes_parsed`. Default:
    /// noop.
    fn on_parsed(&mut self) {}

    /// Recovery instrumentation (default: no-op). The recovering parser
    /// (`recovery.rs`) overrides these to maintain the open bracket-context
    /// stack, so that on a parse error the innermost enclosing
    /// precedence-bounded region can be identified from the parser's
    /// *actual recursion* (approach (a)), not a token re-scan. `on_open_group`
    /// fires after a `(`/`[` opener is consumed; `on_close_group` fires only
    /// on the success path after the matching closer is consumed — an error
    /// inside the group unwinds via `?` without closing it, so the group
    /// remains on the stack and marks the live region at the error point.
    fn on_open_group(&mut self, _opener_tok: usize, _awaits: TokenKind) {}
    fn on_close_group(&mut self) {}
    /// Recovery: `parse_expr`-frame enter/exit, for precedence-valley region
    /// tightening (M3b). `on_exit_expr` fires only on the success path, so an
    /// error leaves the frame (and its ancestors) on the recovering parser's
    /// stack, identifying the live precedence-bounded region.
    fn on_enter_expr(&mut self, _min_prec: u32, _start_tok: usize) {}
    fn on_exit_expr(&mut self) {}

    // --- Trivial methods (default impls) ---
    fn peek(&self) -> &'a Token {
        &self.tokens()[self.pos()]
    }
    fn advance(&mut self) -> &'a Token {
        let i = self.pos();
        let t = &self.tokens()[i];
        self.set_pos(i + 1);
        t
    }
    fn last_end(&self) -> u32 {
        let p = self.pos();
        if p == 0 {
            0
        } else {
            self.tokens()[p - 1].end
        }
    }

    // --- Core parse loop ---
    /// Wrapper recording the frame for recovery instrumentation around the
    /// real body. `on_exit_expr` fires only on success, so an error leaves
    /// the frame on the recovering parser's stack. For the four production
    /// parsers the hooks are no-ops and this is a transparent indirection.
    fn parse_expr(&mut self, min_prec: u32) -> Result<Arc<Node>, ParseError> {
        let start_tok = self.pos();
        self.on_enter_expr(min_prec, start_tok);
        let r = self.parse_expr_inner(min_prec);
        if r.is_ok() {
            self.on_exit_expr();
        }
        r
    }

    fn parse_expr_inner(&mut self, min_prec: u32) -> Result<Arc<Node>, ParseError> {
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

            // Three-way dispatch on Li-Taura's AOPP conflict typology
            // (HPC Asia 2023, §3.3), plus a postfix-operator shortcut
            // for `(`, `[`, `.` which consume specific closers rather
            // than recursing into another `parse_expr(rbp)`.
            if is_postfix(next_kind) {
                left = self.parse_postfix(left, start_byte, min_prec, spine_min)?;
            } else {
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
        let t = self.peek().clone();
        if !is_prefix(t.kind) {
            return match t.kind {
                TokenKind::Eof => Err(ParseError::UnexpectedEof),
                _ => Err(ParseError::UnexpectedToken { kind: t.kind, at: t.start }),
            };
        }
        self.on_parsed();
        match t.kind {
            TokenKind::Int | TokenKind::Ident => {
                self.advance();
                Ok(Arc::new(Node::atom(t.text(self.src()), self.last_end() - start_byte)))
            }
            TokenKind::Minus | TokenKind::Bang => {
                self.advance();
                let child_min_prec = prefix_rbp(t.kind);
                let child = self.parse_expr(child_min_prec)?;
                Ok(Arc::new(Node {
                    kind: NodeKind::Prefix { op: t.kind, child },
                    width: self.last_end() - start_byte,
                    m_spine: u32::MAX,
                    stop_lbp: 0,
                    m_floor: child_min_prec,
                    id: crate::ast::fresh_id(),
                #[cfg(feature = "chain_splice")]
                wb: 1,
                }))
            }
            TokenKind::LParen => {
                let opener = self.pos();
                self.advance();
                self.on_open_group(opener, TokenKind::RParen);
                let inner = self.parse_expr(MIN_PREC)?;
                let close = self.peek().clone();
                if close.kind != TokenKind::RParen {
                    return Err(ParseError::MissingRParen { at: close.start });
                }
                self.advance();
                self.on_close_group();
                Ok(Arc::new(Node {
                    kind: NodeKind::Paren { inner },
                    width: self.last_end() - start_byte,
                    m_spine: u32::MAX,
                    stop_lbp: 0,
                    m_floor: 0,
                    id: crate::ast::fresh_id(),
                #[cfg(feature = "chain_splice")]
                wb: 1,
                }))
            }
            _ => unreachable!(),
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

        if op_kind == TokenKind::Question {
            self.advance();
            let then = self.parse_expr(MIN_PREC)?;
            let colon = self.peek().clone();
            if colon.kind != TokenKind::Colon {
                return Err(ParseError::MissingColon { at: colon.start });
            }
            self.advance();
            let else_ = self.parse_expr(rbp(op_kind))?;
            return Ok(Arc::new(Node {
                kind: NodeKind::Ternary { cond: left, then, else_ },
                width: self.last_end() - outer_start,
                m_spine: outer_spine_min,
                stop_lbp: 0,
                m_floor: outer_min_prec,
                id: crate::ast::fresh_id(),
            #[cfg(feature = "chain_splice")]
            wb: 1,
            }));
        }

        self.advance();
        let right = self.parse_expr(rbp(op_kind))?;
        Ok(Arc::new(Node {
            kind: NodeKind::Binary { op: op_kind, left, right },
            width: self.last_end() - outer_start,
            m_spine: outer_spine_min,
            stop_lbp: 0,
            m_floor: outer_min_prec,
            id: crate::ast::fresh_id(),
        #[cfg(feature = "chain_splice")]
        wb: 1,
        }))
    }

    fn parse_assoc_chain(
        &mut self,
        left: Arc<Node>,
        chain_op: TokenKind,
        outer_start: u32,
    ) -> Result<Arc<Node>, ParseError> {
        let chain_lbp = lbp(chain_op);
        let mut operands: Vec<ChainOperand> = vec![ChainOperand {
            start_byte: outer_start,
            node: left,
        }];
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

    fn parse_postfix(
        &mut self,
        left: Arc<Node>,
        outer_start: u32,
        outer_min_prec: u32,
        outer_spine_min: u32,
    ) -> Result<Arc<Node>, ParseError> {
        let op_tok = self.pos();
        let kind = self.peek().kind;
        self.advance();
        self.on_parsed();
        let new_kind = match kind {
            TokenKind::LParen => {
                self.on_open_group(op_tok, TokenKind::RParen);
                let mut args: Vec<Arc<Node>> = Vec::new();
                if self.peek().kind != TokenKind::RParen {
                    args.push(self.parse_expr(MIN_PREC)?);
                    while self.peek().kind == TokenKind::Comma {
                        self.advance();
                        args.push(self.parse_expr(MIN_PREC)?);
                    }
                }
                let close = self.peek().clone();
                if close.kind != TokenKind::RParen {
                    return Err(ParseError::MissingRParen { at: close.start });
                }
                self.advance();
                self.on_close_group();
                NodeKind::Call { callee: left, args }
            }
            TokenKind::LBracket => {
                self.on_open_group(op_tok, TokenKind::RBracket);
                let index = self.parse_expr(MIN_PREC)?;
                let close = self.peek().clone();
                if close.kind != TokenKind::RBracket {
                    return Err(ParseError::MissingRBracket { at: close.start });
                }
                self.advance();
                self.on_close_group();
                NodeKind::Index { array: left, index }
            }
            TokenKind::Dot => {
                let field_tok = self.peek().clone();
                if field_tok.kind != TokenKind::Ident {
                    return Err(ParseError::MissingMemberName { at: field_tok.start });
                }
                let field: Arc<str> = Arc::from(field_tok.text(self.src()));
                self.advance();
                NodeKind::Member { object: left, field }
            }
            _ => unreachable!(),
        };
        Ok(Arc::new(Node {
            kind: new_kind,
            width: self.last_end() - outer_start,
            m_spine: outer_spine_min,
            stop_lbp: 0,
            m_floor: outer_min_prec,
            id: crate::ast::fresh_id(),
        #[cfg(feature = "chain_splice")]
        wb: 1,
        }))
    }
}
