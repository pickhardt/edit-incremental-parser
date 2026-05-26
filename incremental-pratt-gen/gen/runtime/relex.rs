//! Relex-to-resynchronization (GENERATED-CRATE RUNTIME, grammar-independent).
//!
//! Updates a persistent token store in place after an edit, re-lexing only a
//! window around the edit and reusing the unchanged token suffix (shifted by
//! the edit delta — free, because the store uses relative offsets). Cost is
//! O(edit + resync window), with the linear worst case only when an edit's
//! lexical effect runs to end-of-file (e.g. an unterminated construct) — that
//! bound is fundamental, not an implementation limit.
//!
//! Result equals a full `tokenize` of the new source (the `relex_matches_full`
//! oracle checks this). This keeps a whole-token-stream consumer (syntax
//! highlighting, semantic tokens) current without re-lexing the file. It does
//! NOT feed the parser, which uses demand-driven lexing (`cursor.rs`).
//!
//! Soundness rests on our lexer restarting cleanly at each token boundary
//! (1-byte-context, no lexical modes): re-lexing from the token before the
//! edit re-derives any merge/split across the edit, and once one freshly
//! lexed token matches an old token (same kind, width, and gap) at its
//! shifted position, the entire byte-identical suffix is valid.

use crate::cursor::ByteSource;
use crate::edit::Edit;
use crate::lexer::{lex_token, TokenKind};
use crate::token_store::{TokenSpec, TokenStore};

/// Bring `store` (the tokenization of `old_src`) up to date with the edit,
/// in place, so it becomes the tokenization of `new_src`. Returns the number
/// of tokens re-lexed (the resync window) — for a local edit this is small
/// and independent of file size.
pub fn relex(store: &mut TokenStore, old_src: &str, new_src: &str, edit: &Edit) -> usize {
    relex_into(store, old_src.len(), new_src.as_bytes(), edit)
}

/// Generic core: relex against any `ByteSource` for the new text (a `&[u8]`
/// slice or a rope), given the old length and the edit. Lets the incremental
/// document relex straight from its rope without materializing it.
pub fn relex_into<B: ByteSource + ?Sized>(
    store: &mut TokenStore,
    old_len: usize,
    new_src: &B,
    edit: &Edit,
) -> usize {
    let delta: i64 = new_src.byte_len() as i64 - old_len as i64;
    let e_start = edit.start as u64;
    let e_end = edit.end as u64;

    // Keep tokens [0, s); re-lex from the token before the one containing the
    // edit start (to catch a left-merge across the edit).
    let first = store.index_at_byte(e_start);
    let s = first.saturating_sub(1);
    let relex_from = if s == 0 {
        0u64
    } else {
        store.get(s - 1).map(|t| t.end as u64).unwrap_or(0)
    };

    let mut specs: Vec<TokenSpec> = Vec::new();
    let mut prev_end = relex_from;
    let mut at = relex_from as usize;
    let j: usize;

    loop {
        let t = lex_token(new_src, at);
        let gap = (t.start as u64 - prev_end) as u32;
        let width = t.end - t.start;

        if t.kind == TokenKind::Eof {
            specs.push(TokenSpec { kind: t.kind, gap, width });
            j = store.len(); // replace through the old Eof
            break;
        }

        // Resynchronization: does this freshly lexed token coincide with an
        // old token (shifted by delta) lying strictly after the edit? If so,
        // the old suffix from there on is byte-identical and still valid.
        let b_i = t.start as i64 - delta;
        if b_i >= e_end as i64 {
            let b = b_i as u64;
            let k = store.index_at_byte(b);
            if let (Some(spec_k), Some(tok_k)) = (store.spec_at(k), store.get(k)) {
                if tok_k.start as u64 == b
                    && spec_k.kind == t.kind
                    && spec_k.width == width
                    && spec_k.gap == gap
                {
                    j = k; // keep old[k..]; do not emit this token
                    break;
                }
            }
        }

        specs.push(TokenSpec { kind: t.kind, gap, width });
        prev_end = t.end as u64;
        at = t.end as usize;
    }

    store.splice(s, j, &specs);
    specs.len()
}
