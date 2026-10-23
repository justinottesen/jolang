//! The abstract syntax tree: the structure of the program as it was written.
//!
//! Nodes are stored in flat arrays owned by `Ast` and refer to each other by index, rather than
//! being individually allocated. See "Designing for performance later" in design/compiler.md.

use std::fmt::Write;

use crate::source::{SourceMap, Span};

#[derive(Default)]
pub struct Ast {
    /// Top-level functions, in source order.
    pub fns: Vec<FnDecl>,
    exprs: Vec<Expr>,
}

/// Refers to an expression stored in an `Ast`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExprId(u32);

/// A name as written in the source. Its text is recovered from the span.
#[derive(Clone, Copy, Debug)]
pub struct Ident {
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct FnDecl {
    pub name: Ident,
    pub return_type: TypeExpr,
    /// Always a block expression.
    pub body: ExprId,
    pub span: Span,
}

/// A type as written in the source.
#[derive(Clone, Copy, Debug)]
pub enum TypeExpr {
    /// A type referred to by name, like `u8`.
    Name(Ident),
}

#[derive(Clone, Copy, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub enum ExprKind {
    /// An integer literal. Its value is parsed from the source text by the checker, which knows
    /// what type it needs to fit in.
    Integer,
    /// A block, which evaluates to its final expression.
    Block { tail: ExprId },
}

impl Ast {
    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.exprs[id.0 as usize]
    }

    pub fn add_expr(&mut self, expr: Expr) -> ExprId {
        let id = ExprId(self.exprs.len() as u32);
        self.exprs.push(expr);
        id
    }

    /// Formats the tree for `--emit=ast`, one node per line, with each node's span.
    pub fn dump(&self, sources: &SourceMap) -> String {
        let mut out = String::new();
        for decl in &self.fns {
            let name = sources.text(decl.name.span);
            dump_line(&mut out, 0, &format!("fn `{name}`"), decl.span, sources);
            match decl.return_type {
                TypeExpr::Name(name) => {
                    let label = format!("return type `{}`", sources.text(name.span));
                    dump_line(&mut out, 1, &label, name.span, sources);
                }
            }
            self.dump_expr(&mut out, 1, decl.body, sources);
        }
        out
    }

    fn dump_expr(&self, out: &mut String, depth: usize, id: ExprId, sources: &SourceMap) {
        let expr = self.expr(id);
        match expr.kind {
            ExprKind::Integer => {
                let label = format!("integer `{}`", sources.text(expr.span));
                dump_line(out, depth, &label, expr.span, sources);
            }
            ExprKind::Block { tail } => {
                dump_line(out, depth, "block", expr.span, sources);
                self.dump_expr(out, depth + 1, tail, sources);
            }
        }
    }
}

/// Writes one node as `<label> (<start line>:<column>-<end line>:<column>)`, indented by depth.
fn dump_line(out: &mut String, depth: usize, label: &str, span: Span, sources: &SourceMap) {
    let file = sources.get(span.file);
    let (start_line, start_column) = file.line_col(span.start);
    let (end_line, end_column) = file.line_col(span.end);
    let indent = depth * 2;
    writeln!(out, "{:indent$}{label} ({start_line}:{start_column}-{end_line}:{end_column})", "").unwrap();
}
