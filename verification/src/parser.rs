//! Pratt parser for the calculator grammar.
//!
//! Arena-based parse tree representation: nodes are flat `Copy` records
//! referencing each other by index (NodeId) into a Vec<Node>. This avoids
//! recursive Box destructors that Kani's CBMC backend cannot unwind.
//!
//! Each Node carries (per §3.1 of Paper 2):
//!   - byte span [start, end]
//!   - m_spine: minimum binding power for top-loop spine recursion
//!   - stop_lbp: binding power of the token that terminated this node's parse

use creusot_std::prelude::trusted;

use crate::lexer::{lex, ByteIndex, Token};

pub type BindingPower = i32;
pub type NodeId = usize;

pub const BP_INFINITY: BindingPower = i32::MAX;
pub const BP_NEG_INFINITY: BindingPower = i32::MIN;

// Bounds on parser internal state. Two sets:
//   - Under `cfg(kani)`: tight bounds matching the harness input size (3 bytes).
//     Sized so Kani's CBMC backend completes in reasonable time.
//   - Otherwise (production / regular tests): generous bounds for real use.
// The algorithm is identical; only the bounds differ.

/// Maximum nodes in any parse tree (bounds the stack-allocated arena).
#[cfg(kani)]
pub const MAX_NODES: usize = 8;
#[cfg(not(kani))]
pub const MAX_NODES: usize = 1024;

/// Maximum iterations of parse_expr's top-loop.
#[cfg(kani)]
pub const MAX_PARSE_ITER: usize = 6;
#[cfg(not(kani))]
pub const MAX_PARSE_ITER: usize = 1024;

/// Maximum recursion depth for parse_expr.
#[cfg(kani)]
pub const MAX_PARSE_DEPTH: usize = 4;
#[cfg(not(kani))]
pub const MAX_PARSE_DEPTH: usize = 256;

#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(creusot), derive(PartialEq, Eq))]
pub struct Node {
    pub kind: NodeKind,
    pub start: ByteIndex,
    pub end: ByteIndex,
    pub m_spine: BindingPower,
    pub stop_lbp: BindingPower,
}

impl Node {
    /// A zero-valued placeholder for initializing the fixed-size arena.
    pub const ZERO: Node = Node {
        kind: NodeKind::Int(0),
        start: 0,
        end: 0,
        m_spine: 0,
        stop_lbp: 0,
    };
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(creusot), derive(PartialEq, Eq))]
pub enum NodeKind {
    Int(u8),
    Paren { inner: NodeId },
    Infix { op: Token, left: NodeId, right: NodeId },
    Postfix { op: Token, operand: NodeId },
}

/// Arena-based parse tree, fully stack-allocated for Kani.
/// Nodes are stored flat in a fixed-size array of length MAX_NODES;
/// only the first `len` entries are valid.
#[derive(Debug, Clone, Copy)]
pub struct ParseTree {
    pub nodes: [Node; MAX_NODES],
    pub len: usize,
    pub root: NodeId,
}

impl ParseTree {
    #[trusted]
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    #[trusted]
    pub fn root_node(&self) -> &Node {
        &self.nodes[self.root]
    }
}

/// Parser state: tokens, current position, fixed-size arena being built.
struct Parser<'a> {
    tokens: &'a [(Token, ByteIndex)],
    pos: usize,
    nodes: [Node; MAX_NODES],
    len: usize,
}

impl<'a> Parser<'a> {
    #[trusted]
    fn new(tokens: &'a [(Token, ByteIndex)]) -> Self {
        Parser { tokens, pos: 0, nodes: [Node::ZERO; MAX_NODES], len: 0 }
    }

    #[trusted]
    fn peek(&self) -> Token {
        self.tokens[self.pos].0
    }

    #[trusted]
    fn peek_pos(&self) -> ByteIndex {
        self.tokens[self.pos].1
    }

    #[trusted]
    fn advance(&mut self) {
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
    }

    #[trusted]
    fn push(&mut self, node: Node) -> Option<NodeId> {
        if self.len >= MAX_NODES {
            return None;
        }
        let id = self.len;
        self.nodes[id] = node;
        self.len += 1;
        Some(id)
    }

    /// Parse an expression at minimum binding power `min_prec`.
    /// Returns the parsed NodeId, or None on error.
    /// The top loop is bounded by MAX_PARSE_ITER and recursion by `depth`
    /// (capped at MAX_PARSE_DEPTH) for Kani-tractability.
    #[trusted]
    fn parse_expr(&mut self, min_prec: BindingPower, depth: usize) -> Option<NodeId> {
        if depth >= MAX_PARSE_DEPTH {
            return None;
        }
        let mut left_id = self.parse_prefix(depth)?;

        for _ in 0..MAX_PARSE_ITER {
            let tok = self.peek();
            let lbp = match tok.lbp() {
                Some(p) => p,
                None => {
                    // Not an infix operator; terminate
                    self.nodes[left_id].stop_lbp = BP_NEG_INFINITY;
                    return Some(left_id);
                }
            };

            if lbp <= min_prec {
                self.nodes[left_id].stop_lbp = lbp;
                return Some(left_id);
            }

            let op_pos = self.peek_pos();
            let op_tok = tok;
            self.advance();

            // Update m_spine on the left node: this operator is on our spine
            if lbp < self.nodes[left_id].m_spine {
                self.nodes[left_id].m_spine = lbp;
            }

            if op_tok == Token::Fact {
                let left = self.nodes[left_id];
                let new_node = Node {
                    start: left.start,
                    end: op_pos + 1,
                    m_spine: lbp.min(left.m_spine),
                    stop_lbp: left.stop_lbp,
                    kind: NodeKind::Postfix { op: op_tok, operand: left_id },
                };
                left_id = self.push(new_node)?;
            } else {
                let rbp = op_tok.rbp().unwrap_or(lbp);
                let right_id = self.parse_expr(rbp, depth + 1)?;
                let left = self.nodes[left_id];
                let right = self.nodes[right_id];
                let new_node = Node {
                    start: left.start,
                    end: right.end,
                    m_spine: lbp.min(left.m_spine).min(right.m_spine),
                    stop_lbp: left.stop_lbp,
                    kind: NodeKind::Infix { op: op_tok, left: left_id, right: right_id },
                };
                left_id = self.push(new_node)?;
            }
        }
        // Loop bound reached without termination; input too complex
        None
    }

    #[trusted]
    fn parse_prefix(&mut self, depth: usize) -> Option<NodeId> {
        if depth >= MAX_PARSE_DEPTH {
            return None;
        }
        let tok = self.peek();
        let pos = self.peek_pos();
        match tok {
            Token::Int(n) => {
                self.advance();
                self.push(Node {
                    kind: NodeKind::Int(n),
                    start: pos,
                    end: pos + 1,
                    m_spine: BP_INFINITY,
                    stop_lbp: BP_NEG_INFINITY,
                })
            }
            Token::LParen => {
                self.advance();
                let inner_id = self.parse_expr(BP_NEG_INFINITY, depth + 1)?;
                if self.peek() != Token::RParen {
                    return None;
                }
                let rparen_pos = self.peek_pos();
                self.advance();
                self.push(Node {
                    start: pos,
                    end: rparen_pos + 1,
                    m_spine: BP_INFINITY,
                    stop_lbp: BP_NEG_INFINITY,
                    kind: NodeKind::Paren { inner: inner_id },
                })
            }
            _ => None,
        }
    }
}

/// Top-level batch parse.
#[trusted]
pub fn batch_parse(input: &[u8]) -> Option<ParseTree> {
    let toks = lex(input);
    let mut p = Parser::new(&toks);
    let root = p.parse_expr(BP_NEG_INFINITY, 0)?;
    if p.peek() != Token::Eof {
        return None;
    }
    Some(ParseTree { nodes: p.nodes, len: p.len, root })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_literal() {
        let t = batch_parse(b"3").unwrap();
        assert_eq!(t.root_node().kind, NodeKind::Int(3));
        assert_eq!(t.root_node().start, 0);
        assert_eq!(t.root_node().end, 1);
    }

    #[test]
    fn parse_addition() {
        let t = batch_parse(b"1+2").unwrap();
        match t.root_node().kind {
            NodeKind::Infix { op, .. } => assert_eq!(op, Token::Plus),
            _ => panic!("expected infix"),
        }
        assert_eq!(t.root_node().start, 0);
        assert_eq!(t.root_node().end, 3);
    }

    #[test]
    fn parse_precedence() {
        // 1+2*3 should parse as 1+(2*3)
        let t = batch_parse(b"1+2*3").unwrap();
        match t.root_node().kind {
            NodeKind::Infix { op, right, .. } => {
                assert_eq!(op, Token::Plus);
                match t.node(right).kind {
                    NodeKind::Infix { op, .. } => assert_eq!(op, Token::Times),
                    _ => panic!("expected nested infix"),
                }
            }
            _ => panic!("expected infix at root"),
        }
    }

    #[test]
    fn parse_parens() {
        let t = batch_parse(b"(1+2)*3").unwrap();
        match t.root_node().kind {
            NodeKind::Infix { op, left, .. } => {
                assert_eq!(op, Token::Times);
                match t.node(left).kind {
                    NodeKind::Paren { .. } => {}
                    _ => panic!("expected paren on left"),
                }
            }
            _ => panic!("expected infix at root"),
        }
    }

    #[test]
    fn parse_factorial() {
        let t = batch_parse(b"3!").unwrap();
        match t.root_node().kind {
            NodeKind::Postfix { op, .. } => assert_eq!(op, Token::Fact),
            _ => panic!("expected postfix"),
        }
    }
}
