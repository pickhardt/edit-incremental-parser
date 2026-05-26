//! Kani-verified proof-of-concept for edit-incremental Pratt parsing.
//!
//! See §6 of Paper 2 for the artifact specification and the four harnesses.

pub mod lexer;
pub mod parser;
pub mod incremental;
pub mod recovery;

#[cfg(kani)]
pub mod harnesses;
