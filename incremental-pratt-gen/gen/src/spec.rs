//! Grammar specification schema (the generator's input DSL).
//!
//! A one-page TOML file declaring atoms, a bracket pair, and operators
//! with precedence / fixity / associativity / conflict-class. See
//! `grammars/*.toml`.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Spec {
    pub name: String,
    #[serde(default)]
    pub atoms: Atoms,
    pub paren: Paren,
    #[serde(default)]
    pub prefix: Vec<Prefix>,
    #[serde(default)]
    pub postfix: Vec<Postfix>,
    #[serde(default)]
    pub infix: Vec<Infix>,
    /// Optional statement/declaration grammar wrapping the expression core.
    /// When omitted, the generator supplies a default `let`/expr/block grammar.
    pub host: Option<Host>,
}

/// A statement-grammar specification: a small EBNF-ish set of productions over
/// literal tokens, identifiers, and the embedded `expr` nonterminal. Each
/// alternative is a whitespace-separated list of terms:
///   `'literal'`  — a literal token (keyword if alphanumeric, else punctuation)
///   `ident`      — an identifier atom
///   `expr`       — an embedded Pratt expression (delimited by the next literal)
///   `Name`       — a reference to production `Name`
///   `Name*`      — zero or more of production `Name` (until the next literal)
#[derive(Debug, Deserialize)]
pub struct Host {
    /// The start production's name.
    pub start: String,
    /// Reserved words (an `ident` term will not match these). Literal tokens
    /// that are alphanumeric are added automatically.
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(rename = "prod")]
    pub prods: Vec<HostProd>,
}

#[derive(Debug, Deserialize)]
pub struct HostProd {
    pub name: String,
    /// Each alternative is a term-string (see `Host`).
    pub alts: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Atoms {
    /// Lex runs of ASCII digits as `Int`.
    #[serde(default)]
    pub int: bool,
    /// Lex identifier runs (`[A-Za-z_][A-Za-z0-9_]*`) as `Ident`.
    #[serde(default)]
    pub ident: bool,
}

#[derive(Debug, Deserialize)]
pub struct Paren {
    pub open: String,
    pub close: String,
}

#[derive(Debug, Deserialize)]
pub struct Prefix {
    pub sym: String,
    /// min_prec passed to the operand parse. Conventionally above every
    /// infix lbp so `-a + b` is `(-a) + b`.
    pub rbp: u32,
}

#[derive(Debug, Deserialize)]
pub struct Postfix {
    pub sym: String,
    /// Binding power at which the loop absorbs this postfix operator.
    pub lbp: u32,
}

#[derive(Debug, Deserialize)]
pub struct Infix {
    pub sym: String,
    pub lbp: u32,
    #[serde(default = "default_assoc")]
    pub assoc: String, // "left" | "right"
    #[serde(default = "default_class")]
    pub class: String, // "weak" | "assoc" | "strong"
}

fn default_assoc() -> String { "left".into() }
fn default_class() -> String { "weak".into() }

impl Infix {
    /// Right-binding power: `lbp` for left-assoc, `lbp - 1` for right-assoc.
    pub fn rbp(&self) -> u32 {
        if self.assoc == "right" { self.lbp.saturating_sub(1) } else { self.lbp }
    }
    pub fn class_variant(&self) -> &'static str {
        match self.class.as_str() {
            "assoc" | "assoc_conflict" | "associativity_conflict" => "AssociativityConflict",
            "strong" => "Strong",
            _ => "Weak",
        }
    }
}
