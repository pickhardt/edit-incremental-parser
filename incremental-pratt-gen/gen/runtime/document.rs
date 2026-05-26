//! Incremental document (GENERATED-CRATE RUNTIME, grammar-independent).
//!
//! The capstone of the persistent token store (`token_store.rs`) and
//! relex-to-resync (`relex.rs`): a live document holding the source text in
//! a **rope** and the token stream in the spliceable store, both updated per
//! edit in O(log n) text splice + O(edit + resync) relex — no full re-copy,
//! no full re-tokenize. This is what an editor integration holds to keep
//! syntax highlighting / semantic tokens current on every keystroke.
//!
//! The lexer reads the rope through `ByteSource` (so relex never materializes
//! the rope into a contiguous buffer). The parser is unchanged and is not
//! involved here — it uses demand-driven lexing over a `&[u8]` snapshot.

use ropey::Rope;

use crate::cursor::{tokenize, ByteSource};
use crate::edit::Edit;
use crate::lexer::Token;
use crate::relex::relex_into;
use crate::token_store::TokenStore;

/// A `ByteSource` view over a rope, so `lex_token` / `relex_into` can read
/// the document text without copying it into a contiguous buffer.
struct RopeBytes<'a>(&'a Rope);

impl ByteSource for RopeBytes<'_> {
    #[inline]
    fn byte_len(&self) -> usize {
        self.0.len_bytes()
    }
    #[inline]
    fn byte_at(&self, i: usize) -> u8 {
        self.0.byte(i)
    }
}

/// Source text (rope) + token stream (store), kept in sync incrementally.
pub struct IncrementalDocument {
    rope: Rope,
    store: TokenStore,
}

impl IncrementalDocument {
    pub fn new(src: &str) -> Self {
        let store = TokenStore::from_tokens(&tokenize(src));
        IncrementalDocument { rope: Rope::from_str(src), store }
    }

    /// Replace byte range `[start, end)` with `replacement`. Splices the rope
    /// (O(log n)) and relexes the token stream against the rope
    /// (O(edit + resync)). Returns the relex window size (tokens re-lexed).
    pub fn edit(&mut self, start: usize, end: usize, replacement: &str) -> usize {
        let old_len = self.rope.len_bytes();
        // ropey mutates by char index; convert from byte offsets (== byte
        // offsets for ASCII, which the generated lexers assume).
        let c_start = self.rope.byte_to_char(start);
        let c_end = self.rope.byte_to_char(end);
        self.rope.remove(c_start..c_end);
        self.rope.insert(c_start, replacement);

        let edit = Edit { start: start as u32, end: end as u32, replacement: replacement.to_string() };
        relex_into(&mut self.store, old_len, &RopeBytes(&self.rope), &edit)
    }

    /// The current token stream (with absolute positions). O(n) — for
    /// whole-stream consumers (highlighting); they would instead read the
    /// changed range incrementally in practice.
    pub fn tokens(&self) -> Vec<Token> {
        self.store.to_vec()
    }

    /// The current source text. O(n) materialization (rare; the rope is the
    /// live representation).
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn byte_len(&self) -> usize {
        self.rope.len_bytes()
    }
}
