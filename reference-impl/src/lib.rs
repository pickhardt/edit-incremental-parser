//! Incremental Pratt parsing — precedence-bounded subtree reuse.
//!
//! See `top_down_locality_principle_outline.md` for the paper outline,
//! `README.md` for the POC overview.

pub mod ast;
#[cfg(feature = "chain_splice")]
mod chain_wb;
pub mod bench_support;
pub mod edit;
pub mod incremental;
pub mod lexer;
pub mod op;
pub mod parser;
pub mod pratt_core;
pub mod recovery;
pub mod roslyn_style;
#[cfg(feature = "node_id")]
pub mod semantics;
#[cfg(feature = "node_id")]
pub mod semantics_ctx;
pub mod span_lookahead;

pub use ast::Node;
pub use edit::Edit;
pub use incremental::{incremental_parse, incremental_parse_with_cache, ReparseStats, ReuseCache};
pub use lexer::{tokenize, Token, TokenKind};
pub use parser::parse;
pub use recovery::{recover_parse, Cost, Repair, RepairCosts};
pub use roslyn_style::{roslyn_style_parse, roslyn_style_with_cache};
pub use span_lookahead::{span_lookahead_parse, span_lookahead_with_cache};
