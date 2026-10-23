//! The high-level intermediate representation: what the program means, after checking.
//!
//! Compared to the AST, names are resolved, every expression has a type, and literals have values.
//! A HIR only exists for a program that passed checking, so later stages can rely on it being
//! valid. Like the AST, nodes are stored in flat arrays and refer to each other by index.

use std::fmt::{self, Write};

use crate::intern::{Interner, Symbol};
use crate::source::{SourceMap, Span};

#[derive(Default)]
pub struct Hir {
    pub fns: Vec<Function>,
    exprs: Vec<Expr>,
}

/// Refers to an expression stored in a `Hir`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExprId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    U8,
}

impl Type {
    /// The largest value of an integer type.
    pub fn max_value(self) -> u128 {
        match self {
            Type::U8 => u8::MAX.into(),
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let name = match self {
            Type::U8 => "u8",
        };
        f.write_str(name)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Function {
    pub name: Symbol,
    pub return_type: Type,
    /// Always a block expression.
    pub body: ExprId,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Type,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub enum ExprKind {
    /// An integer constant. The value is guaranteed to fit in the expression's type.
    Integer { value: u128 },
    /// A block, which evaluates to its final expression.
    Block { tail: ExprId },
}

impl Hir {
    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.exprs[id.0 as usize]
    }

    pub fn add_expr(&mut self, expr: Expr) -> ExprId {
        let id = ExprId(self.exprs.len() as u32);
        self.exprs.push(expr);
        id
    }

    /// Formats the HIR for `--emit=hir`, one node per line, with each node's type and span.
    pub fn dump(&self, sources: &SourceMap, symbols: &Interner) -> String {
        let mut out = String::new();
        for function in &self.fns {
            let name = symbols.get(function.name);
            let location = sources.span_location(function.span);
            writeln!(out, "fn `{name}` -> {} ({location})", function.return_type).unwrap();
            self.dump_expr(&mut out, 1, function.body, sources);
        }
        out
    }

    fn dump_expr(&self, out: &mut String, depth: usize, id: ExprId, sources: &SourceMap) {
        let expr = self.expr(id);
        let label = match expr.kind {
            ExprKind::Integer { value } => format!("integer {value}"),
            ExprKind::Block { .. } => "block".to_string(),
        };
        let location = sources.span_location(expr.span);
        let indent = depth * 2;
        writeln!(out, "{:indent$}{label}: {} ({location})", "", expr.ty).unwrap();
        if let ExprKind::Block { tail } = expr.kind {
            self.dump_expr(out, depth + 1, tail, sources);
        }
    }
}
