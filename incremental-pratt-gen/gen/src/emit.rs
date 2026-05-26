//! Codegen backend: Spec -> a standalone Rust parser crate.
//!
//! Emits the three grammar-specific modules (`lexer`, `op`,
//! `grammar_text`) plus `Cargo.toml`, and copies the shared runtime
//! verbatim. The runtime is byte-identical across grammars, so a reviewer
//! can `diff out/<g1>/src/op.rs out/<g2>/src/op.rs` and see only the
//! grammar specialise.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::spec::Spec;

// Shared runtime, embedded at generator build time.
const RT_AST: &str = include_str!("../runtime/ast.rs");
const RT_EDIT: &str = include_str!("../runtime/edit.rs");
const RT_PARSER: &str = include_str!("../runtime/parser.rs");
const RT_PRATT: &str = include_str!("../runtime/pratt_core.rs");
const RT_INCR: &str = include_str!("../runtime/incremental.rs");
const RT_LIB: &str = include_str!("../runtime/lib.rs");
const RT_ORACLE: &str = include_str!("../runtime/tests_oracle.rs");
const RT_BENCH: &str = include_str!("../runtime/bin_bench.rs");
const RT_VERIFY: &str = include_str!("../runtime/verify.rs");
const RT_CURSOR: &str = include_str!("../runtime/cursor.rs");
const RT_DOCUMENT: &str = include_str!("../runtime/document.rs");
const RT_HOST: &str = include_str!("../runtime/host.rs");
const RT_TOKEN_STORE: &str = include_str!("../runtime/token_store.rs");
const RT_RELEX: &str = include_str!("../runtime/relex.rs");
const CREUSOT_LIB_TEMPLATE: &str = include_str!("../runtime/verify_creusot.rs.tmpl");

/// Pinned Creusot upstream commit; `creusot-std` must match the installed
/// `cargo-creusot` (see verification/README.md).
const CREUSOT_REV: &str = "34e1aecf46060e9479b803a85476022eeeed4728";

/// why3find configuration (ported verbatim from the verification artifact).
const WHY3FIND_JSON: &str = r#"{
  "fast": 0.5,
  "time": 10,
  "depth": 8,
  "packages": [ "creusot" ],
  "provers": [ "alt-ergo", "z3", "cvc5", "cvc4" ],
  "tactics": [ "compute_specified", "split_vc" ],
  "drivers": [],
  "warnoff": [ "unused_variable", "axiom_abstract" ]
}
"#;

/// Role flags accumulated per distinct symbol string.
#[derive(Default, Clone)]
struct Roles {
    variant: String,
    prefix_rbp: Option<u32>,
    postfix_lbp: Option<u32>,
    infix_lbp: Option<u32>,
    infix_rbp: Option<u32>,
    infix_class: Option<&'static str>,
    open_paren: bool,
    close_paren: bool,
}

/// A resolved term in a host-grammar alternative (production refs by index).
#[derive(Debug)]
enum HTerm {
    Tok(String),
    Ident,
    Expr,
    Nt(usize),
    Rep(usize),
}

#[derive(Debug)]
struct HProd {
    name: String,
    alts: Vec<Vec<HTerm>>,
}

/// The resolved statement grammar: productions (NT refs as indices), the start
/// index, reserved words, and the punctuation bytes used as the expr-reuse
/// guard.
#[derive(Debug)]
struct HostModel {
    prods: Vec<HProd>,
    start: usize,
    keywords: Vec<String>,
    punct_bytes: Vec<u8>,
}

pub struct Emitter {
    spec: Spec,
    /// symbol -> roles, in declaration-stable order.
    syms: BTreeMap<String, Roles>,
    /// resolved statement grammar (user-supplied or the default).
    host: HostModel,
}

impl Emitter {
    pub fn new(spec: Spec) -> Result<Self, String> {
        let mut syms: BTreeMap<String, Roles> = BTreeMap::new();
        let ensure = |s: &str, syms: &mut BTreeMap<String, Roles>| {
            syms.entry(s.to_string()).or_insert_with(|| Roles {
                variant: sym_to_variant(s),
                ..Default::default()
            });
        };

        ensure(&spec.paren.open, &mut syms);
        syms.get_mut(&spec.paren.open).unwrap().open_paren = true;
        ensure(&spec.paren.close, &mut syms);
        syms.get_mut(&spec.paren.close).unwrap().close_paren = true;

        for p in &spec.prefix {
            ensure(&p.sym, &mut syms);
            syms.get_mut(&p.sym).unwrap().prefix_rbp = Some(p.rbp);
        }
        for p in &spec.postfix {
            ensure(&p.sym, &mut syms);
            syms.get_mut(&p.sym).unwrap().postfix_lbp = Some(p.lbp);
        }
        for inf in &spec.infix {
            ensure(&inf.sym, &mut syms);
            let r = syms.get_mut(&inf.sym).unwrap();
            r.infix_lbp = Some(inf.lbp);
            r.infix_rbp = Some(inf.rbp());
            r.infix_class = Some(inf.class_variant());
        }

        // Reject the one genuinely ambiguous overlap: a symbol that is both
        // infix and postfix would make the loop dispatch undecidable.
        for (s, r) in &syms {
            if r.infix_lbp.is_some() && r.postfix_lbp.is_some() {
                return Err(format!("symbol {:?} declared as both infix and postfix", s));
            }
        }
        if !spec.atoms.int && !spec.atoms.ident {
            return Err("grammar must enable at least one atom kind (int or ident)".into());
        }
        let host = build_host_model(&spec, &syms)?;
        Ok(Emitter { spec, syms, host })
    }

    fn variant_list(&self) -> Vec<&str> {
        self.syms.values().map(|r| r.variant.as_str()).collect()
    }

    pub fn emit(&self, out_dir: &Path) -> std::io::Result<()> {
        let src = out_dir.join("src");
        fs::create_dir_all(src.join("bin"))?;
        fs::create_dir_all(out_dir.join("tests"))?;

        // Copied runtime (byte-identical across grammars).
        fs::write(src.join("ast.rs"), RT_AST)?;
        fs::write(src.join("edit.rs"), RT_EDIT)?;
        fs::write(src.join("parser.rs"), RT_PARSER)?;
        fs::write(src.join("pratt_core.rs"), RT_PRATT)?;
        fs::write(src.join("incremental.rs"), RT_INCR)?;
        fs::write(src.join("lib.rs"), RT_LIB)?;
        fs::write(src.join("cursor.rs"), RT_CURSOR)?;
        fs::write(src.join("document.rs"), RT_DOCUMENT)?;
        fs::write(src.join("host.rs"), RT_HOST)?;
        fs::write(src.join("token_store.rs"), RT_TOKEN_STORE)?;
        fs::write(src.join("relex.rs"), RT_RELEX)?;
        fs::write(src.join("verify.rs"), RT_VERIFY)?;
        fs::write(src.join("bin").join("bench.rs"), RT_BENCH)?;
        fs::write(out_dir.join("tests").join("oracle.rs"), RT_ORACLE)?;

        // Generated, grammar-specific.
        fs::write(src.join("lexer.rs"), self.gen_lexer())?;
        fs::write(src.join("op.rs"), self.gen_op())?;
        fs::write(src.join("grammar_text.rs"), self.gen_grammar_text())?;
        fs::write(src.join("host_grammar.rs"), self.gen_host_grammar())?;
        fs::write(out_dir.join("Cargo.toml"), self.gen_cargo_toml())?;

        // Creusot unbounded certificate, as a self-contained sibling crate.
        // Only for single-byte-operator grammars: the logic-level
        // next-token model (Pearlite) handles single-byte tokens, matching
        // paper 2's calculator-grammar restriction (where conditions 3 and
        // 4 of Definition 3.3 coincide). Multi-byte longest-match would
        // need a much heavier logic spec and is left to the Kani tier.
        if self.all_single_byte() {
            let c = out_dir.join("creusot");
            fs::create_dir_all(c.join("src"))?;
            fs::write(c.join("Cargo.toml"), self.gen_creusot_cargo_toml())?;
            fs::write(c.join("src").join("lib.rs"), self.gen_creusot_lib())?;
            fs::write(c.join("why3find.json"), WHY3FIND_JSON)?;
        }
        Ok(())
    }

    /// True iff every operator symbol is a single byte — the precondition
    /// for the single-byte Creusot logic model.
    fn all_single_byte(&self) -> bool {
        self.syms.keys().all(|s| s.len() == 1)
    }

    fn gen_cargo_toml(&self) -> String {
        format!(
            "# GENERATED by incremental-pratt-gen from grammars/{name}.toml\n\
             [package]\n\
             name = \"ip_{name}\"\n\
             version = \"0.1.0\"\n\
             edition = \"2021\"\n\n\
             [lib]\n\
             name = \"ipgrammar\"\n\
             path = \"src/lib.rs\"\n\n\
             [[bin]]\n\
             name = \"bench\"\n\
             path = \"src/bin/bench.rs\"\n\n\
             [dependencies]\n\
             rustc-hash = \"2\"\n\
             ropey = \"1\"\n\n\
             [dev-dependencies]\n\
             proptest = \"1\"\n\n\
             [profile.release]\n\
             debug = true\n\n\
             [lints.rust]\n\
             unexpected_cfgs = {{ level = \"allow\", check-cfg = ['cfg(kani)'] }}\n",
            name = self.spec.name
        )
    }

    fn gen_creusot_cargo_toml(&self) -> String {
        format!(
            "# GENERATED Creusot certificate crate for grammar `{name}`.\n\
             # Verify with: cargo creusot   (expects `Proved (N files)`)\n\
             [package]\n\
             name = \"ip_{name}_creusot\"\n\
             version = \"0.1.0\"\n\
             edition = \"2021\"\n\n\
             [lib]\n\
             name = \"ipgrammar_creusot\"\n\
             path = \"src/lib.rs\"\n\n\
             [dependencies]\n\
             creusot-std = {{ git = \"https://github.com/creusot-rs/creusot\", rev = \"{rev}\" }}\n\n\
             [lints.rust]\n\
             unexpected_cfgs = {{ level = \"allow\", check-cfg = ['cfg(creusot)'] }}\n",
            name = self.spec.name,
            rev = CREUSOT_REV,
        )
    }

    fn gen_creusot_lib(&self) -> String {
        // Single-byte operator -> lbp (infix or postfix), deduped.
        let mut sym_lbp: Vec<(u8, u32)> = self
            .syms
            .iter()
            .filter_map(|(s, r)| r.infix_lbp.or(r.postfix_lbp).map(|l| (s.as_bytes()[0], l)))
            .collect();
        sym_lbp.sort();
        sym_lbp.dedup();

        let mut logic = String::new();
        for (b, l) in &sym_lbp {
            logic.push_str(&format!("if b == {}u8 {{ {} }} else ", b, l));
        }
        logic.push_str("{ 0 }"); // final else branch (pearlite needs braces)
        let mut op = String::new();
        for (b, l) in &sym_lbp {
            op.push_str(&format!("{}u8 => return {},\n            ", b, l));
        }

        CREUSOT_LIB_TEMPLATE
            .replace("/*@GRAMMAR@*/", &self.spec.name)
            .replace("/*@LBP_OF_BYTE_LOGIC@*/", &logic)
            .replace("/*@NEXT_TOKEN_LBP_OP@*/", &op)
    }

    fn gen_lexer(&self) -> String {
        let mut variants = String::new();
        if self.spec.atoms.int {
            variants.push_str("    Int,\n");
        }
        if self.spec.atoms.ident {
            variants.push_str("    Ident,\n");
        }
        for v in self.variant_list() {
            variants.push_str(&format!("    {},\n", v));
        }

        // Longest-match arms: all symbols sorted by length descending so a
        // multi-byte operator wins over its single-byte prefix.
        let mut sym_pairs: Vec<(&String, &Roles)> = self.syms.iter().collect();
        sym_pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(b.0)));
        let mut match_arms = String::new();
        for (sym, r) in &sym_pairs {
            match_arms.push_str(&format!(
                "            if matches_at(src, i, {lit}) {{ return Token {{ kind: TokenKind::{v}, start: i as u32, end: (i + {n}) as u32 }}; }}\n",
                lit = byte_literal(sym),
                v = r.variant,
                n = sym.len(),
            ));
        }

        let int_arm = if self.spec.atoms.int {
            "            if b.is_ascii_digit() {\n\
            \x20               let mut j = i + 1;\n\
            \x20               while j < src.byte_len() && src.byte_at(j).is_ascii_digit() { j += 1; }\n\
            \x20               return Token { kind: TokenKind::Int, start: i as u32, end: j as u32 };\n\
            \x20           }\n"
        } else {
            ""
        };
        let ident_arm = if self.spec.atoms.ident {
            "            if b == b'_' || b.is_ascii_alphabetic() {\n\
            \x20               let mut j = i + 1;\n\
            \x20               while j < src.byte_len() && (src.byte_at(j) == b'_' || src.byte_at(j).is_ascii_alphanumeric()) { j += 1; }\n\
            \x20               return Token { kind: TokenKind::Ident, start: i as u32, end: j as u32 };\n\
            \x20           }\n"
        } else {
            ""
        };

        let (pieces, seed, grow) = self.corpus();

        format!(
            "//! GENERATED lexer for grammar `{name}`.\n\
             //!\n\
             //! `lex_token` returns the first token at or after a byte offset,\n\
             //! skipping whitespace and unknown bytes (longest-match on operators).\n\
             //! The parser pulls tokens on demand via `crate::cursor::Lexer`, so a\n\
             //! reused subtree's interior is never lexed.\n\n\
             #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]\n\
             pub enum TokenKind {{\n{variants}    Eof,\n}}\n\n\
             #[derive(Debug, Clone, Copy, PartialEq, Eq)]\n\
             pub struct Token {{\n    pub kind: TokenKind,\n    pub start: u32,\n    pub end: u32,\n}}\n\n\
             impl Token {{\n    pub fn text<'a>(&self, src: &'a str) -> &'a str {{\n        &src[self.start as usize..self.end as usize]\n    }}\n}}\n\n\
             use crate::cursor::ByteSource;\n\n\
             fn matches_at<B: ByteSource + ?Sized>(src: &B, i: usize, prefix: &[u8]) -> bool {{\n\
             \x20   if i + prefix.len() > src.byte_len() {{ return false; }}\n\
             \x20   let mut k = 0;\n\
             \x20   while k < prefix.len() {{ if src.byte_at(i + k) != prefix[k] {{ return false; }} k += 1; }}\n\
             \x20   true\n}}\n\n\
             /// First token at or after byte `at`; `Eof` at end of input.\n\
             /// Generic over the byte source so it serves both a `&[u8]` slice\n\
             /// (parser) and a rope (incremental document) unchanged.\n\
             pub fn lex_token<B: ByteSource + ?Sized>(src: &B, at: usize) -> Token {{\n\
             \x20   let mut i = at;\n\
             \x20   loop {{\n\
             \x20       while i < src.byte_len() && matches!(src.byte_at(i), b' ' | b'\\t' | b'\\n' | b'\\r') {{ i += 1; }}\n\
             \x20       if i >= src.byte_len() {{ let n = src.byte_len() as u32; return Token {{ kind: TokenKind::Eof, start: n, end: n }}; }}\n\
             \x20       let b = src.byte_at(i);\n\
             {match_arms}\
             {int_arm}\
             {ident_arm}\
             \x20       // Unknown byte: skip and keep scanning.\n\
             \x20       i += 1;\n\
             \x20   }}\n\
             }}\n\n\
             /// Corpus for the soundness oracle and bench (single-token pieces).\n\
             pub const PIECES: &[&str] = &[{pieces}];\n\
             /// A minimal valid expression and a growth suffix that keeps it valid.\n\
             pub const SEED_EXPR: &str = {seed};\n\
             pub const GROW: &str = {grow};\n\
             /// Building blocks for a balanced nested bench source.\n\
             pub const ATOM: &str = {atom};\n\
             pub const INFIX: &str = {infix};\n\
             pub const POPEN: &str = {popen};\n\
             pub const PCLOSE: &str = {pclose};\n",
            name = self.spec.name,
            atom = format!("{:?}", if self.spec.atoms.int { "1" } else { "a" }),
            infix = format!("{:?}", self.syms.iter().find(|(_, r)| r.infix_lbp.is_some()).map(|(s, _)| s.as_str()).unwrap_or("+")),
            popen = format!("{:?}", self.spec.paren.open),
            pclose = format!("{:?}", self.spec.paren.close),
        )
    }

    fn gen_op(&self) -> String {
        let mut lbp_arms = String::new();
        let mut rbp_arms = String::new();
        let mut prefix_rbp_arms = String::new();
        let mut class_arms = String::new();
        let mut prefix_variants = Vec::new();
        let mut postfix_variants = Vec::new();
        let mut open_variant = String::new();
        let mut close_variant = String::new();

        for r in self.syms.values() {
            if let Some(l) = r.infix_lbp {
                lbp_arms.push_str(&format!("        TokenKind::{} => {},\n", r.variant, l));
                rbp_arms.push_str(&format!("        TokenKind::{} => {},\n", r.variant, r.infix_rbp.unwrap()));
                class_arms.push_str(&format!(
                    "        TokenKind::{} => OperatorClass::{},\n",
                    r.variant,
                    r.infix_class.unwrap()
                ));
            }
            if let Some(l) = r.postfix_lbp {
                lbp_arms.push_str(&format!("        TokenKind::{} => {},\n", r.variant, l));
            }
            if let Some(rb) = r.prefix_rbp {
                prefix_rbp_arms.push_str(&format!("        TokenKind::{} => {},\n", r.variant, rb));
                prefix_variants.push(r.variant.clone());
            }
            if r.postfix_lbp.is_some() {
                postfix_variants.push(r.variant.clone());
            }
            if r.open_paren {
                open_variant = r.variant.clone();
            }
            if r.close_paren {
                close_variant = r.variant.clone();
            }
        }

        let prefix_pat = or_pattern(&prefix_variants);
        let postfix_pat = or_pattern(&postfix_variants);

        // Byte-level next-token lbp, for the verification harness (operates
        // on symbolic &[u8], so it cannot go through the &str lexer). Skips
        // whitespace, then longest-matches an operator and returns its lbp;
        // atoms and unknown bytes have lbp 0 (LBP_NONE).
        let mut sym_lbp: Vec<(&String, u32)> = self
            .syms
            .iter()
            .map(|(s, r)| (s, r.infix_lbp.or(r.postfix_lbp).unwrap_or(0)))
            .collect();
        sym_lbp.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(b.0)));
        let mut ntl_arms = String::new();
        for (sym, l) in &sym_lbp {
            ntl_arms.push_str(&format!(
                "    if matches_at(src, pos, {lit}) {{ return {l}; }}\n",
                lit = byte_literal(sym),
            ));
        }

        format!(
            "//! GENERATED operator tables for grammar `{name}`.\n\n\
             use crate::lexer::TokenKind;\n\n\
             pub const MIN_PREC: u32 = 0;\n\
             pub const LBP_NONE: u32 = 0;\n\n\
             #[derive(Debug, Clone, Copy, PartialEq, Eq)]\n\
             pub enum OperatorClass {{ Strong, Weak, AssociativityConflict }}\n\n\
             /// Left-binding power (infix + postfix operators); 0 otherwise.\n\
             pub fn lbp(kind: TokenKind) -> u32 {{\n    match kind {{\n{lbp_arms}        _ => LBP_NONE,\n    }}\n}}\n\n\
             /// Right-binding power for infix operators (min_prec of the right operand).\n\
             pub fn rbp(kind: TokenKind) -> u32 {{\n    match kind {{\n{rbp_arms}        _ => lbp(kind),\n    }}\n}}\n\n\
             /// min_prec passed to a prefix operator's operand parse.\n\
             pub fn prefix_rbp(kind: TokenKind) -> u32 {{\n    match kind {{\n{prefix_rbp_arms}        _ => 0,\n    }}\n}}\n\n\
             /// AOPP conflict class of an infix operator.\n\
             pub fn operator_class(kind: TokenKind) -> OperatorClass {{\n    match kind {{\n{class_arms}        _ => OperatorClass::Strong,\n    }}\n}}\n\n\
             pub fn is_atom(kind: TokenKind) -> bool {{ matches!(kind, {atom_pat}) }}\n\
             pub fn is_prefix_op(kind: TokenKind) -> bool {{ matches!(kind, {prefix_pat}) }}\n\
             pub fn is_postfix_op(kind: TokenKind) -> bool {{ matches!(kind, {postfix_pat}) }}\n\
             pub fn is_open_paren(kind: TokenKind) -> bool {{ matches!(kind, TokenKind::{open}) }}\n\
             pub fn close_paren_kind() -> TokenKind {{ TokenKind::{close} }}\n\
             pub fn is_associativity_conflict(kind: TokenKind) -> bool {{\n    matches!(operator_class(kind), OperatorClass::AssociativityConflict)\n}}\n\n\
             fn matches_at(src: &[u8], i: usize, prefix: &[u8]) -> bool {{\n    src.len() >= i + prefix.len() && &src[i..i + prefix.len()] == prefix\n}}\n\n\
             /// Left-binding power of the first non-whitespace token at or after\n\
             /// `pos` in `src` (byte level; for the verification harness).\n\
             pub fn next_token_lbp(src: &[u8], mut pos: usize) -> u32 {{\n\
             \x20   while pos < src.len() && matches!(src[pos], b' ' | b'\\t' | b'\\n' | b'\\r') {{ pos += 1; }}\n\
             \x20   if pos >= src.len() {{ return LBP_NONE; }}\n\
             {ntl_arms}\
             \x20   LBP_NONE\n\
             }}\n",
            name = self.spec.name,
            atom_pat = self.atom_pattern(),
            open = open_variant,
            close = close_variant,
        )
    }

    fn atom_pattern(&self) -> String {
        let mut v = Vec::new();
        if self.spec.atoms.int { v.push("Int".to_string()); }
        if self.spec.atoms.ident { v.push("Ident".to_string()); }
        or_pattern(&v)
    }

    /// Emit the statement-grammar table consumed by the runtime `host` module.
    fn gen_host_grammar(&self) -> String {
        let term_src = |t: &HTerm| -> String {
            match t {
                HTerm::Tok(s) => format!("Term::Tok({:?})", s),
                HTerm::Ident => "Term::Ident".into(),
                HTerm::Expr => "Term::Expr".into(),
                HTerm::Nt(i) => format!("Term::Nt({})", i),
                HTerm::Rep(i) => format!("Term::Rep({})", i),
            }
        };
        let mut prods = String::new();
        for p in &self.host.prods {
            let mut alts = String::new();
            for alt in &p.alts {
                let terms = alt.iter().map(term_src).collect::<Vec<_>>().join(", ");
                alts.push_str(&format!("        Alt {{ terms: &[{}] }},\n", terms));
            }
            prods.push_str(&format!(
                "    Production {{ name: {:?}, alts: &[\n{}    ] }},\n",
                p.name, alts
            ));
        }
        let keywords = self
            .host
            .keywords
            .iter()
            .map(|k| format!("{:?}", k))
            .collect::<Vec<_>>()
            .join(", ");
        let punct = self
            .host
            .punct_bytes
            .iter()
            .map(|b| format!("{}u8", b))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "//! GENERATED statement grammar for `{name}`.\n\
             //!\n\
             //! Consumed by the grammar-independent `crate::host` interpreter +\n\
             //! incremental-reuse engine. Only this table is grammar-specific.\n\n\
             use crate::host::{{Alt, Grammar, Production, Term}};\n\n\
             pub const GRAMMAR: Grammar = Grammar {{\n\
             \x20   start: {start},\n\
             \x20   keywords: &[{keywords}],\n\
             \x20   punct_bytes: &[{punct}],\n\
             \x20   prods: &[\n{prods}    ],\n\
             }};\n\n\
             /// A valid program covering every statement alternative, for the\n\
             /// generic host round-trip oracle.\n\
             pub const SAMPLE_PROGRAM: &str = {sample};\n",
            name = self.spec.name,
            start = self.host.start,
            sample = format!("{:?}", self.sample_program()),
        )
    }

    /// Synthesize one valid program covering every alternative of the start
    /// production's statement element — so the generic oracle round-trips the
    /// *whole* statement grammar (e.g. `print`/`def`), not just the subset it
    /// can generate blind.
    fn sample_program(&self) -> String {
        let atom = if self.spec.atoms.int { "1" } else { "a" };
        // Minimal alternative of production `i`: the first with no Nt/Rep term
        // (so expansion terminates), else alt 0.
        let minimal = |i: usize| -> usize {
            let p = &self.host.prods[i];
            p.alts
                .iter()
                .position(|a| !a.iter().any(|t| matches!(t, HTerm::Nt(_) | HTerm::Rep(_))))
                .unwrap_or(0)
        };
        fn expand_alt(
            this: &Emitter,
            terms: &[HTerm],
            atom: &str,
            depth: usize,
            minimal: &dyn Fn(usize) -> usize,
            cover_rep: bool,
            out: &mut String,
        ) {
            for t in terms {
                match t {
                    HTerm::Tok(s) => {
                        out.push_str(s);
                        out.push(' ');
                    }
                    HTerm::Ident => out.push_str("a "),
                    HTerm::Expr => {
                        out.push_str(atom);
                        out.push(' ');
                    }
                    HTerm::Nt(i) => {
                        let ai = minimal(*i);
                        expand_alt(this, &this.host.prods[*i].alts[ai], atom, depth + 1, minimal, false, out);
                    }
                    HTerm::Rep(i) => {
                        if cover_rep && depth == 0 {
                            // Cover every alternative of the repeated production.
                            for alt in &this.host.prods[*i].alts {
                                expand_alt(this, alt, atom, depth + 1, minimal, false, out);
                            }
                        } else {
                            let ai = minimal(*i);
                            expand_alt(this, &this.host.prods[*i].alts[ai], atom, depth + 1, minimal, false, out);
                        }
                    }
                }
            }
        }
        let start = self.host.start;
        let mut out = String::new();
        // Use the start production's first alternative, covering its rep.
        expand_alt(self, &self.host.prods[start].alts[0], atom, 0, &minimal, true, &mut out);
        out
    }

    fn gen_grammar_text(&self) -> String {
        let mut arms = String::new();
        if self.spec.atoms.int { arms.push_str("        TokenKind::Int => \"<int>\",\n"); }
        if self.spec.atoms.ident { arms.push_str("        TokenKind::Ident => \"<id>\",\n"); }
        for (sym, r) in &self.syms {
            arms.push_str(&format!("        TokenKind::{} => {:?},\n", r.variant, sym));
        }
        format!(
            "//! GENERATED display text for grammar `{name}`.\n\n\
             use crate::lexer::TokenKind;\n\n\
             pub fn token_text(t: TokenKind) -> &'static str {{\n    match t {{\n{arms}        TokenKind::Eof => \"<eof>\",\n    }}\n}}\n",
            name = self.spec.name,
        )
    }

    /// Build the oracle/bench corpus: single-token PIECES plus a valid
    /// SEED_EXPR and a length-growing, validity-preserving GROW suffix.
    fn corpus(&self) -> (String, String, String) {
        let atom_samples: Vec<&str> = if self.spec.atoms.int {
            vec!["1", "2", "7"]
        } else {
            vec!["a", "b", "x"]
        };
        let mut pieces: Vec<String> = Vec::new();
        for s in &atom_samples {
            pieces.push(s.to_string());
        }
        for sym in self.syms.keys() {
            pieces.push(sym.clone());
        }
        pieces.push(" ".to_string());
        let pieces_lit = pieces.iter().map(|p| format!("{:?}", p)).collect::<Vec<_>>().join(", ");

        let atom = atom_samples[0];
        let atom2 = *atom_samples.get(1).unwrap_or(&atom);
        // Build a growth unit that introduces *reusable* structure: an
        // infix-glued parenthesized subexpression. The parenthesised group
        // is a Paren node with band [stop_lbp, +inf), reusable as an operand
        // of the outer chain — the typical case, unlike a flat operator
        // chain (the Diekmann pathology) whose interior nodes have empty
        // bands. Falls back to bare infix, then postfix, then nothing.
        let infix = self.syms.iter().find(|(_, r)| r.infix_lbp.is_some()).map(|(s, _)| s.clone());
        let grow = match (&infix, &self.spec.paren.open, &self.spec.paren.close) {
            (Some(op), open, close) => format!("{op}{open}{atom}{op}{atom2}{close}"),
            _ => match self.syms.iter().find(|(_, r)| r.postfix_lbp.is_some()) {
                Some((sym, _)) => sym.clone(),
                None => String::new(),
            },
        };
        (pieces_lit, format!("{:?}", atom), format!("{:?}", grow))
    }
}

/// Resolve the host grammar (the spec's `[host]` or a default `let`/expr/block
/// grammar) into productions with NT refs as indices, validating that it is a
/// sound recursive-descent grammar for the embedded expression language.
fn build_host_model(spec: &Spec, syms: &BTreeMap<String, Roles>) -> Result<HostModel, String> {
    // (start, keywords, [(name, [alt-string])]) — user-supplied or default.
    let (start_name, mut keywords, raw): (String, Vec<String>, Vec<(String, Vec<String>)>) =
        match &spec.host {
            Some(h) => (
                h.start.clone(),
                h.keywords.clone(),
                h.prods.iter().map(|p| (p.name.clone(), p.alts.clone())).collect(),
            ),
            None => (
                "program".into(),
                vec!["let".into()],
                vec![
                    ("program".into(), vec!["stmt*".into()]),
                    (
                        "stmt".into(),
                        vec![
                            "'let' ident '=' expr ';'".into(),
                            "'{' stmt* '}'".into(),
                            "expr ';'".into(),
                        ],
                    ),
                ],
            ),
        };

    if raw.is_empty() {
        return Err("host grammar has no productions".into());
    }
    let names: Vec<String> = raw.iter().map(|(n, _)| n.clone()).collect();
    let index_of = |name: &str| -> Result<usize, String> {
        names
            .iter()
            .position(|n| n == name)
            .ok_or_else(|| format!("host grammar references undefined production {:?}", name))
    };
    let start = index_of(&start_name)?;

    // Expression-token bytes: a host literal that scans as an expr terminator
    // must not collide with any of these (else `scan_to` would stop early).
    let mut expr_bytes: Vec<u8> = Vec::new();
    for s in syms.keys() {
        expr_bytes.extend_from_slice(s.as_bytes());
    }
    if spec.atoms.int {
        expr_bytes.extend(b'0'..=b'9');
    }
    if spec.atoms.ident {
        expr_bytes.extend(b'a'..=b'z');
        expr_bytes.extend(b'A'..=b'Z');
        expr_bytes.push(b'_');
    }

    let mut punct_bytes: Vec<u8> = Vec::new();
    let mut prods: Vec<HProd> = Vec::with_capacity(raw.len());
    for (pi, (name, alts)) in raw.iter().enumerate() {
        let mut palts: Vec<Vec<HTerm>> = Vec::new();
        for alt in alts {
            let toks: Vec<&str> = alt.split_whitespace().collect();
            if toks.is_empty() {
                return Err(format!("production {:?} has an empty alternative", name));
            }
            let mut terms: Vec<HTerm> = Vec::with_capacity(toks.len());
            for t in &toks {
                let term = if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') {
                    let lit = t[1..t.len() - 1].to_string();
                    if lit.is_empty() {
                        return Err(format!("empty literal in production {:?}", name));
                    }
                    if lit.bytes().all(|b| b == b'_' || b.is_ascii_alphanumeric()) {
                        if !keywords.contains(&lit) {
                            keywords.push(lit.clone());
                        }
                    } else {
                        for b in lit.bytes() {
                            if !punct_bytes.contains(&b) {
                                punct_bytes.push(b);
                            }
                        }
                    }
                    HTerm::Tok(lit)
                } else if *t == "expr" {
                    HTerm::Expr
                } else if *t == "ident" {
                    HTerm::Ident
                } else if let Some(nm) = t.strip_suffix('*') {
                    HTerm::Rep(index_of(nm)?)
                } else {
                    HTerm::Nt(index_of(t)?)
                };
                terms.push(term);
            }
            palts.push(terms);
        }
        // Terminator soundness: Expr/Rep must be followed by a literal, except
        // when last in the start production (terminated by EOF).
        for alt in &palts {
            for (ti, term) in alt.iter().enumerate() {
                if matches!(term, HTerm::Expr | HTerm::Rep(_)) {
                    match alt.get(ti + 1) {
                        Some(HTerm::Tok(lit)) => {
                            if lit.bytes().any(|b| expr_bytes.contains(&b)) {
                                return Err(format!(
                                    "terminator {:?} in production {:?} collides with an expression token byte",
                                    lit, name
                                ));
                            }
                        }
                        None if pi == start => { /* EOF terminator, sound at top level */ }
                        _ => {
                            return Err(format!(
                                "expr/repetition in production {:?} must be followed by a literal terminator",
                                name
                            ));
                        }
                    }
                }
            }
        }
        prods.push(HProd { name: name.clone(), alts: palts });
    }

    // LL(1) dispatch check (resolving nonterminal heads through FIRST sets):
    // each production's alternatives must have pairwise-distinct first literals
    // and at most one "open" (non-literal-headed) alternative.
    for p in &prods {
        let mut seen_lits: Vec<String> = Vec::new();
        let mut open_count = 0;
        for alt in &p.alts {
            let (lits, open) = alt_first(&prods, alt.first(), 0);
            if open {
                open_count += 1;
            }
            for l in lits {
                if seen_lits.contains(&l) {
                    return Err(format!(
                        "production {:?} is ambiguous: two alternatives both start with {:?}",
                        p.name, l
                    ));
                }
                seen_lits.push(l);
            }
        }
        if open_count > 1 {
            return Err(format!(
                "production {:?} has {} alternatives with no leading literal (need <=1 for LL(1) dispatch)",
                p.name, open_count
            ));
        }
    }

    punct_bytes.sort_unstable();
    punct_bytes.dedup();
    Ok(HostModel { prods, start, keywords, punct_bytes })
}

/// FIRST set of an alternative led by `term`: its possible leading literals
/// (resolving nonterminal heads) and whether it can begin without a literal.
fn alt_first(prods: &[HProd], term: Option<&HTerm>, depth: usize) -> (Vec<String>, bool) {
    if depth > prods.len() {
        return (Vec::new(), false); // cycle guard
    }
    match term {
        None => (Vec::new(), true),
        Some(HTerm::Tok(s)) => (vec![s.clone()], false),
        Some(HTerm::Ident) | Some(HTerm::Expr) | Some(HTerm::Rep(_)) => (Vec::new(), true),
        Some(HTerm::Nt(i)) => {
            let mut lits = Vec::new();
            let mut open = false;
            for alt in &prods[*i].alts {
                let (l, o) = alt_first(prods, alt.first(), depth + 1);
                lits.extend(l);
                open |= o;
            }
            (lits, open)
        }
    }
}

/// `a | b | c` as a Rust or-pattern over `TokenKind` variants; `_ if false`
/// (never matches) when empty.
fn or_pattern(variants: &[String]) -> String {
    if variants.is_empty() {
        return "_ if false".to_string();
    }
    variants
        .iter()
        .map(|v| format!("TokenKind::{}", v))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// `b"&&"`-style byte-string literal for the lexer's `matches_at`.
fn byte_literal(s: &str) -> String {
    format!("b{:?}", s)
}

/// Map an operator symbol to a readable, unique `TokenKind` variant ident.
fn sym_to_variant(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        out.push_str(match c {
            '+' => "Plus",
            '-' => "Minus",
            '*' => "Star",
            '/' => "Slash",
            '%' => "Percent",
            '^' => "Caret",
            '!' => "Bang",
            '&' => "Amp",
            '|' => "Pipe",
            '=' => "Eq",
            '<' => "Lt",
            '>' => "Gt",
            '~' => "Tilde",
            '?' => "Question",
            ':' => "Colon",
            '.' => "Dot",
            '@' => "At",
            '#' => "Hash",
            '$' => "Dollar",
            '(' => "LParen",
            ')' => "RParen",
            '[' => "LBracket",
            ']' => "RBracket",
            '{' => "LBrace",
            '}' => "RBrace",
            other => return format!("Tok{}", other as u32), // last-resort fallback
        });
    }
    out
}
