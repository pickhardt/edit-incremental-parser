//! GENERATED incremental Pratt parser.
//!
//! Emitted by incremental-pratt-gen from a one-page grammar specification.
//! Grammar-specific modules (`lexer`, `op`, `grammar_text`) are generated;
//! the rest (`ast`, `edit`, `parser`, `pratt_core`, `incremental`) is the
//! shared runtime, identical across all generated grammars.

pub mod ast;
pub mod cursor;
pub mod document;
pub mod edit;
pub mod grammar_text;
pub mod host;
pub mod host_grammar;
pub mod incremental;
pub mod lexer;
pub mod op;
pub mod parser;
pub mod pratt_core;
pub mod relex;
pub mod token_store;
pub mod verify;

pub use ast::{Node, NodeKind};
pub use cursor::{tokenize, ByteSource, Lexer};
pub use document::IncrementalDocument;
pub use host::{parse_program, reparse_program, Program};
pub use relex::{relex, relex_into};
pub use token_store::{TokenSpec, TokenStore};
pub use edit::Edit;
pub use incremental::{
    incremental_parse, incremental_parse_with_cache, incremental_reparse, ReparseStats, ReuseCache,
};
pub use lexer::{lex_token, Token, TokenKind};
pub use parser::{parse, ParseError};
