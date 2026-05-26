//! Lexer for the calculator grammar.
//!
//! Tokens are single-character lexed (1-byte-context greedy), matching the
//! assumption in §3.2's tokenization-boundary condition.

#[allow(unused_imports)]
use creusot_std::{
    logic::{Int, Seq},
    prelude::{ensures, logic, pearlite, requires, trusted, variant},
};
#[cfg(creusot)]
use creusot_std::std::cmp::PartialEq;

pub type ByteIndex = usize;

/// Sentinel for "no token / unrecognized / not an infix operator."
/// Matches `parser::BP_NEG_INFINITY` but defined here to avoid the
/// circular import that would arise from referring to it from this file.
pub const BP_NEG_INFINITY_I32: i32 = i32::MIN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(creusot, derive(creusot_std::model::DeepModel))]
pub enum Token {
    /// Integer literal (single decimal digit 0-9)
    Int(u8),
    Plus,
    Minus,
    Times,
    Divide,
    Pow,
    Fact,
    LParen,
    RParen,
    /// Sentinel for end-of-input
    Eof,
}

impl Token {
    /// Tokens that are infix operators have a left binding power (lbp).
    /// Higher lbp = binds more tightly.
    /// Returns None for tokens that are not infix operators.
    #[trusted]
    pub fn lbp(self) -> Option<i32> {
        match self {
            Token::Plus | Token::Minus => Some(10),
            Token::Times | Token::Divide => Some(20),
            Token::Pow => Some(30),       // right-associative; rbp = 29 below
            Token::Fact => Some(40),      // postfix
            _ => None,
        }
    }

    /// Right binding power for infix parse_expr recursion.
    /// For right-associative operators, rbp = lbp - 1 (so equal-precedence
    /// rhs is included in the recursion).
    #[trusted]
    pub fn rbp(self) -> Option<i32> {
        match self {
            Token::Plus | Token::Minus => Some(10),
            Token::Times | Token::Divide => Some(20),
            Token::Pow => Some(29),  // right-associative
            Token::Fact => None,     // postfix has no right operand
            _ => None,
        }
    }
}

// ---- Logic-level model of the lexer's next-token-lbp lookup -----------

/// Logic-level mapping from byte to lbp for the calculator grammar.
/// Mirrors the byte-to-token-to-lbp pipeline of `lex` + `Token::lbp`
/// for the single-byte tokens used by the calculator grammar.
/// Returns `i32::MIN` (BP_NEG_INFINITY) for any byte that does not
/// correspond to an infix operator (digits, parens, whitespace, unknown).
#[logic(open)]
pub fn lbp_of_byte_logic(b: u8) -> Int {
    pearlite! {
        if b == 43u8 || b == 45u8 { 10 }          // '+', '-'
        else if b == 42u8 || b == 47u8 { 20 }     // '*', '/'
        else if b == 94u8 { 30 }                   // '^'
        else if b == 33u8 { 40 }                   // '!'
        else { i32::MIN@ }                         // BP_NEG_INFINITY
    }
}

/// Logic-level next-token-lbp lookup: scan `src` from `pos`, skip
/// whitespace bytes (space, tab, newline), return the lbp of the first
/// non-whitespace byte (or `i32::MIN` if `pos` is past the end or no
/// non-whitespace byte exists).
///
/// Mirrors the operational `next_token_lbp` below.
#[logic(open)]
#[variant(src.len() - pos)]
#[requires(0 <= pos)]
pub fn next_token_lbp_logic(src: Seq<u8>, pos: Int) -> Int {
    pearlite! {
        if pos >= src.len() {
            i32::MIN@
        } else if src[pos] == 32u8 || src[pos] == 9u8 || src[pos] == 10u8 {
            next_token_lbp_logic(src, pos + 1)
        } else {
            lbp_of_byte_logic(src[pos])
        }
    }
}

/// Scan `src` starting at byte position `pos`, skip whitespace, and
/// return the lbp of the next non-whitespace token (or `BP_NEG_INFINITY`
/// if no such token exists before end-of-input). Used by `reuse_predicate`
/// (condition 4 of Definition 3.2) to check next-token left-binding-power
/// stability across an edit.
#[trusted]
#[requires(pos@ <= src@.len())]
#[ensures(result@ == next_token_lbp_logic(src@, pos@))]
pub fn next_token_lbp(src: &[u8], pos: usize) -> i32 {
    let mut i = pos;
    while i < src.len() {
        let b = src[i];
        match b {
            b' ' | b'\t' | b'\n' => { i += 1; }
            b'+' | b'-' => return 10,
            b'*' | b'/' => return 20,
            b'^' => return 30,
            b'!' => return 40,
            _ => return BP_NEG_INFINITY_I32,
        }
    }
    BP_NEG_INFINITY_I32
}

// ---- Operational lexer ------------------------------------------------

/// Lex a byte slice into a vector of (token, start_index) pairs.
/// Whitespace is ignored. Unrecognized bytes produce Eof early.
#[trusted]
pub fn lex(input: &[u8]) -> Vec<(Token, ByteIndex)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        let tok = match b {
            b' ' | b'\t' | b'\n' => {
                i += 1;
                continue;
            }
            b'0'..=b'9' => Token::Int(b - b'0'),
            b'+' => Token::Plus,
            b'-' => Token::Minus,
            b'*' => Token::Times,
            b'/' => Token::Divide,
            b'^' => Token::Pow,
            b'!' => Token::Fact,
            b'(' => Token::LParen,
            b')' => Token::RParen,
            _ => {
                // Unrecognized byte: stop lexing (Kani harnesses constrain to valid bytes anyway)
                break;
            }
        };
        out.push((tok, i));
        i += 1;
    }
    out.push((Token::Eof, i));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lex_simple() {
        let toks = lex(b"1+2");
        assert_eq!(toks.len(), 4); // 1, +, 2, Eof
        assert_eq!(toks[0].0, Token::Int(1));
        assert_eq!(toks[1].0, Token::Plus);
        assert_eq!(toks[2].0, Token::Int(2));
        assert_eq!(toks[3].0, Token::Eof);
    }

    #[test]
    fn lex_with_whitespace() {
        let toks = lex(b"1 + 2");
        assert_eq!(toks.len(), 4);
        assert_eq!(toks[0].1, 0);
        assert_eq!(toks[1].1, 2); // position of '+'
        assert_eq!(toks[2].1, 4); // position of '2'
    }

    #[test]
    fn lex_parens() {
        let toks = lex(b"(1)");
        assert_eq!(toks.len(), 4);
        assert_eq!(toks[0].0, Token::LParen);
        assert_eq!(toks[1].0, Token::Int(1));
        assert_eq!(toks[2].0, Token::RParen);
    }
}
