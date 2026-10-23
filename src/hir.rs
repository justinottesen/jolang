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
    stmts: Vec<Stmt>,
}

/// Refers to an expression stored in a `Hir`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExprId(u32);

/// Refers to a contiguous run of statements stored in a `Hir`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StmtRange {
    start: u32,
    len: u32,
}

/// Refers to a local variable within its function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    /// `()`: one value, carrying no information.
    Unit,
    U8,
}

impl Type {
    /// The largest value of an integer type, or `None` if the type isn't an integer.
    pub fn max_value(self) -> Option<u128> {
        match self {
            Type::Unit => None,
            Type::U8 => Some(u8::MAX.into()),
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let name = match self {
            Type::Unit => "()",
            Type::U8 => "u8",
        };
        f.write_str(name)
    }
}

#[derive(Clone, Debug)]
pub struct Function {
    pub name: Symbol,
    pub return_type: Type,
    /// Every local variable declared anywhere in the function.
    pub locals: Vec<Local>,
    /// Always a block expression.
    pub body: ExprId,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct Local {
    pub name: Symbol,
    pub ty: Type,
    pub mutable: bool,
    /// Where the variable's name is written in its `let`.
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub enum Stmt {
    /// Declares `local`, and initializes it with the value of `init`.
    Let { local: LocalId, init: ExprId },
    /// Evaluates an expression of type `()`.
    Expr(ExprId),
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
    /// Reads a local variable. As the target of an assignment, refers to the variable instead.
    Local(LocalId),
    /// Statements, then optionally a final expression that gives the block its value.
    Block { stmts: StmtRange, tail: Option<ExprId> },
    /// Stores `value` in `target`, which is always a `Local` expression for now.
    Assign { target: ExprId, value: ExprId },
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

    pub fn stmts(&self, range: StmtRange) -> &[Stmt] {
        &self.stmts[range.start as usize..(range.start + range.len) as usize]
    }

    /// Stores a block's statements together, so the block can refer to them as one range.
    pub fn add_stmts(&mut self, stmts: Vec<Stmt>) -> StmtRange {
        let range = StmtRange { start: self.stmts.len() as u32, len: stmts.len() as u32 };
        self.stmts.extend(stmts);
        range
    }

    /// Formats the HIR for `--emit=hir`, one node per line, with each node's type and span.
    pub fn dump(&self, sources: &SourceMap, symbols: &Interner) -> String {
        let mut out = String::new();
        for function in &self.fns {
            let name = symbols.get(function.name);
            let location = sources.span_location(function.span);
            writeln!(out, "fn `{name}` -> {} ({location})", function.return_type).unwrap();
            for local in &function.locals {
                let mutable = if local.mutable { "mut " } else { "" };
                let location = sources.span_location(local.span);
                writeln!(out, "  local {mutable}`{}`: {} ({location})", symbols.get(local.name), local.ty).unwrap();
            }
            let mut dumper = Dumper { hir: self, function, sources, symbols, out: &mut out };
            dumper.expr(1, function.body);
        }
        out
    }
}

struct Dumper<'a> {
    hir: &'a Hir,
    function: &'a Function,
    sources: &'a SourceMap,
    symbols: &'a Interner,
    out: &'a mut String,
}

impl Dumper<'_> {
    fn line(&mut self, depth: usize, label: &str, span: Span) {
        let location = self.sources.span_location(span);
        let indent = depth * 2;
        writeln!(self.out, "{:indent$}{label} ({location})", "").unwrap();
    }

    fn local_name(&self, local: LocalId) -> &str {
        self.symbols.get(self.function.locals[local.0 as usize].name)
    }

    fn expr(&mut self, depth: usize, id: ExprId) {
        let expr = *self.hir.expr(id);
        let label = match expr.kind {
            ExprKind::Integer { value } => format!("integer {value}"),
            ExprKind::Local(local) => format!("local `{}`", self.local_name(local)),
            ExprKind::Block { .. } => "block".to_string(),
            ExprKind::Assign { .. } => "assign".to_string(),
        };
        self.line(depth, &format!("{label}: {}", expr.ty), expr.span);
        match expr.kind {
            ExprKind::Integer { .. } | ExprKind::Local(_) => {}
            ExprKind::Block { stmts, tail } => {
                for stmt in self.hir.stmts(stmts) {
                    self.stmt(depth + 1, *stmt);
                }
                if let Some(tail) = tail {
                    self.expr(depth + 1, tail);
                }
            }
            ExprKind::Assign { target, value } => {
                self.expr(depth + 1, target);
                self.expr(depth + 1, value);
            }
        }
    }

    fn stmt(&mut self, depth: usize, stmt: Stmt) {
        match stmt {
            Stmt::Let { local, init } => {
                let label = format!("let `{}`", self.local_name(local));
                let span = self.function.locals[local.0 as usize].span;
                self.line(depth, &label, span);
                self.expr(depth + 1, init);
            }
            Stmt::Expr(expr) => {
                let span = self.hir.expr(expr).span;
                self.line(depth, "statement", span);
                self.expr(depth + 1, expr);
            }
        }
    }
}
