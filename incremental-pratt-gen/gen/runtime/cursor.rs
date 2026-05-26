//! Incremental lexer cursor (GENERATED-CRATE RUNTIME, grammar-independent).
//!
//! Instead of tokenizing the whole source up front, the parser pulls tokens
//! from this cursor one at a time via `peek`/`advance`. On a reuse, the
//! parser calls `reposition` to jump the cursor past the reused subtree —
//! so the interior of a reused subtree is never lexed. Lexing work then
//! tracks what the parser actually visits (the reparsed region), not the
//! file size.
//!
//! The grammar-specific single-token lexer is `crate::lexer::lex_token`,
//! which returns the first token at or after a byte offset (skipping
//! whitespace and unknown bytes), or `Eof` at end of input.

use crate::lexer::{lex_token, Token, TokenKind};

/// A byte-addressable source the lexer can read from. Implemented for a
/// contiguous `[u8]` (the parser's fast path) and for a rope (the
/// incremental document, `document.rs`), so the same generated `lex_token`
/// serves both without materializing the rope into a contiguous buffer.
pub trait ByteSource {
    fn byte_len(&self) -> usize;
    fn byte_at(&self, i: usize) -> u8;
}

impl ByteSource for [u8] {
    #[inline]
    fn byte_len(&self) -> usize {
        self.len()
    }
    #[inline]
    fn byte_at(&self, i: usize) -> u8 {
        self[i]
    }
}

pub struct Lexer<'a> {
    bytes: &'a [u8],
    cur: Token,
    last_end: u32,
    lexed: u32,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        let bytes = src.as_bytes();
        let cur = lex_token(bytes, 0);
        Lexer { bytes, cur, last_end: 0, lexed: 1 }
    }

    /// The current (lookahead) token. O(1).
    pub fn peek(&self) -> Token {
        self.cur
    }

    /// Consume the current token and lex the next one. O(token length).
    pub fn advance(&mut self) -> Token {
        let t = self.cur;
        self.last_end = t.end;
        self.cur = lex_token(self.bytes, t.end as usize);
        self.lexed += 1;
        t
    }

    /// End byte of the most recently consumed token (0 before any advance).
    pub fn last_end(&self) -> u32 {
        self.last_end
    }

    /// Jump the cursor to `byte` and lex the next token there. Used on a
    /// reuse to skip the reused subtree without lexing its interior.
    pub fn reposition(&mut self, byte: u32) {
        self.last_end = byte;
        self.cur = lex_token(self.bytes, byte as usize);
        self.lexed += 1;
    }

    /// Number of `lex_token` calls so far — i.e. lexing work done.
    pub fn lexed(&self) -> u32 {
        self.lexed
    }
}

/// Full tokenization, built by repeated `lex_token`. Used only to build the
/// reuse cache from the previous tree (amortized once per loaded tree), not
/// on the per-edit hot path.
pub fn tokenize(src: &str) -> Vec<Token> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut at = 0usize;
    loop {
        let t = lex_token(bytes, at);
        let is_eof = matches!(t.kind, TokenKind::Eof);
        let end = t.end as usize;
        out.push(t);
        if is_eof {
            break;
        }
        at = end;
    }
    out
}
