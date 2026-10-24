//! The high-level intermediate representation: what the program means, after checking.
//!
//! Compared to the AST, names are resolved, every expression has a type, and literals have values.
//! A HIR only exists for a program that passed checking, so later stages can rely on it being
//! valid. Like the AST, nodes are stored in flat arrays and refer to each other by index.

use std::fmt::{self, Write};

use crate::ast::{BinaryOp, UnaryOp};
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
    Bool,
    Int(IntType),
}

impl Type {
    pub const U8: Type = Type::Int(IntType { signed: false, bits: 8 });
}

/// An integer type. Signed integers are two's complement. Only 8, 16, 32, and 64 bits are allowed
/// for now, but nothing here depends on that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntType {
    pub signed: bool,
    pub bits: u16,
}

impl IntType {
    pub fn min(self) -> i128 {
        if self.signed { -(1 << (self.bits - 1)) } else { 0 }
    }

    pub fn max(self) -> i128 {
        if self.signed { (1 << (self.bits - 1)) - 1 } else { (1 << self.bits) - 1 }
    }

    /// Whether every value of `other` is also a value of this type.
    pub fn contains(self, other: IntType) -> bool {
        self.min() <= other.min() && other.max() <= self.max()
    }
}

impl fmt::Display for IntType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}{}", if self.signed { 'i' } else { 'u' }, self.bits)
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Type::Unit => f.write_str("()"),
            Type::Bool => f.write_str("bool"),
            Type::Int(int) => int.fmt(f),
        }
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
    Integer { value: i128 },
    /// The unit value, `()`.
    Unit,
    Bool(bool),
    /// Evaluates `cond`, then one branch. Without an `else`, the expression has type `()`.
    If { cond: ExprId, then_branch: ExprId, else_branch: Option<ExprId> },
    /// Reads a local variable. As the target of an assignment, refers to the variable instead.
    Local(LocalId),
    /// Statements, then optionally a final expression that gives the block its value.
    Block { stmts: StmtRange, tail: Option<ExprId> },
    /// Stores `value` in `target`, which is always a `Local` expression for now.
    Assign { target: ExprId, value: ExprId },
    /// Negation of a signed integer, or logical not of a `bool`. The operand has the expression's
    /// type.
    Unary { op: UnaryOp, operand: ExprId },
    /// A binary operator. Both operands have the same type: the expression's type for arithmetic
    /// and logical operators, and an integer or `bool` for comparisons, whose result is `bool`.
    Binary { op: BinaryOp, lhs: ExprId, rhs: ExprId },
    /// Converts an integer to the expression's type: `as%` if `wrapping`, otherwise `as`.
    Cast { operand: ExprId, wrapping: bool },
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
            ExprKind::Unit => "unit".to_string(),
            ExprKind::Bool(value) => format!("bool {value}"),
            ExprKind::If { .. } => "if".to_string(),
            ExprKind::Block { .. } => "block".to_string(),
            ExprKind::Assign { .. } => "assign".to_string(),
            ExprKind::Unary { op, .. } => format!("unary `{}`", op.symbol()),
            ExprKind::Binary { op, .. } => format!("binary `{}`", op.symbol()),
            ExprKind::Cast { wrapping, .. } => if wrapping { "cast `as%`" } else { "cast `as`" }.to_string(),
        };
        self.line(depth, &format!("{label}: {}", expr.ty), expr.span);
        match expr.kind {
            ExprKind::Integer { .. } | ExprKind::Unit | ExprKind::Bool(_) | ExprKind::Local(_) => {}
            ExprKind::If { cond, then_branch, else_branch } => {
                self.expr(depth + 1, cond);
                self.expr(depth + 1, then_branch);
                if let Some(else_branch) = else_branch {
                    self.expr(depth + 1, else_branch);
                }
            }
            ExprKind::Unary { operand, .. } | ExprKind::Cast { operand, .. } => self.expr(depth + 1, operand),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.expr(depth + 1, lhs);
                self.expr(depth + 1, rhs);
            }
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
