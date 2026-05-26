//! Minimal lexer for the expression language.
//!
//! Tokens carry byte spans `[start, end)` into the source. The lexer is
//! whitespace-skipping and produces a final `Eof` token whose span is
//! `[len, len)`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // Literals & names
    Int,
    Ident,
    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    Bang,
    AndAnd,
    OrOr,
    EqEq,
    BangEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    Question,
    Colon,
    // Grouping
    LParen,
    RParen,
    // Postfix-related
    LBracket,
    RBracket,
    Dot,
    Comma,
    // Sentinel
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub start: u32,
    pub end: u32,
}

impl Token {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start as usize..self.end as usize]
    }
}

pub fn tokenize(src: &str) -> Vec<Token> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        // Skip whitespace
        if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
            i += 1;
            continue;
        }
        let start = i as u32;
        let (kind, next) = match b {
            b'+' => (TokenKind::Plus, i + 1),
            b'-' => (TokenKind::Minus, i + 1),
            b'*' => (TokenKind::Star, i + 1),
            b'/' => (TokenKind::Slash, i + 1),
            b'%' => (TokenKind::Percent, i + 1),
            b'^' => (TokenKind::Caret, i + 1),
            b'?' => (TokenKind::Question, i + 1),
            b':' => (TokenKind::Colon, i + 1),
            b'(' => (TokenKind::LParen, i + 1),
            b')' => (TokenKind::RParen, i + 1),
            b'[' => (TokenKind::LBracket, i + 1),
            b']' => (TokenKind::RBracket, i + 1),
            b'.' => (TokenKind::Dot, i + 1),
            b',' => (TokenKind::Comma, i + 1),
            b'&' if matches_at(bytes, i, b"&&") => (TokenKind::AndAnd, i + 2),
            b'|' if matches_at(bytes, i, b"||") => (TokenKind::OrOr, i + 2),
            b'=' if matches_at(bytes, i, b"==") => (TokenKind::EqEq, i + 2),
            b'!' if matches_at(bytes, i, b"!=") => (TokenKind::BangEq, i + 2),
            b'!' => (TokenKind::Bang, i + 1),
            b'<' if matches_at(bytes, i, b"<=") => (TokenKind::LtEq, i + 2),
            b'>' if matches_at(bytes, i, b">=") => (TokenKind::GtEq, i + 2),
            b'<' => (TokenKind::Lt, i + 1),
            b'>' => (TokenKind::Gt, i + 1),
            b'0'..=b'9' => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                (TokenKind::Int, j)
            }
            b'_' | b'a'..=b'z' | b'A'..=b'Z' => {
                let mut j = i + 1;
                while j < bytes.len()
                    && (bytes[j] == b'_' || bytes[j].is_ascii_alphanumeric())
                {
                    j += 1;
                }
                (TokenKind::Ident, j)
            }
            _ => {
                // Unknown byte — skip. (Parser will not consume it.)
                i += 1;
                continue;
            }
        };
        out.push(Token {
            kind,
            start,
            end: next as u32,
        });
        i = next;
    }
    out.push(Token {
        kind: TokenKind::Eof,
        start: bytes.len() as u32,
        end: bytes.len() as u32,
    });
    out
}

fn matches_at(bytes: &[u8], i: usize, prefix: &[u8]) -> bool {
    bytes.len() >= i + prefix.len() && &bytes[i..i + prefix.len()] == prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lex_arith() {
        let toks = tokenize("a + b * 12");
        let kinds: Vec<_> = toks.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Ident,
                TokenKind::Plus,
                TokenKind::Ident,
                TokenKind::Star,
                TokenKind::Int,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_multichar() {
        let toks = tokenize("x && y || z != 0");
        let kinds: Vec<_> = toks.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Ident,
                TokenKind::AndAnd,
                TokenKind::Ident,
                TokenKind::OrOr,
                TokenKind::Ident,
                TokenKind::BangEq,
                TokenKind::Int,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_spans() {
        let toks = tokenize("a+b");
        assert_eq!(toks[0].start, 0);
        assert_eq!(toks[0].end, 1);
        assert_eq!(toks[1].start, 1);
        assert_eq!(toks[1].end, 2);
        assert_eq!(toks[2].start, 2);
        assert_eq!(toks[2].end, 3);
    }
}
