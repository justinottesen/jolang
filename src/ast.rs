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
    stmts: Vec<Stmt>,
}

/// Refers to an expression stored in an `Ast`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExprId(u32);

/// Refers to a contiguous run of statements stored in an `Ast`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StmtRange {
    start: u32,
    len: u32,
}

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
    /// The unit type, `()`.
    Unit(Span),
}

impl TypeExpr {
    pub fn span(self) -> Span {
        match self {
            TypeExpr::Name(name) => name.span,
            TypeExpr::Unit(span) => span,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Stmt {
    /// `let mut? name (: ty)? = init;`
    Let { mutable: bool, name: Ident, ty: Option<TypeExpr>, init: ExprId, span: Span },
    /// An expression used as a statement. `semicolon` is false for a block-like expression written
    /// without one.
    Expr { expr: ExprId, semicolon: bool },
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
    /// A name, which the checker resolves to a variable.
    Name(Ident),
    /// `()`, the unit value.
    Unit,
    /// `true` or `false`.
    Bool(bool),
    /// `if cond { ... } else ...`. `then_branch` is a block; `else_branch` is a block or another
    /// `if`.
    If { cond: ExprId, then_branch: ExprId, else_branch: Option<ExprId> },
    /// A block: statements, then optionally a final expression that gives the block its value.
    Block { stmts: StmtRange, tail: Option<ExprId> },
    /// `target = value`.
    Assign { target: ExprId, value: ExprId },
    Unary { op: UnaryOp, operand: ExprId },
    Binary { op: BinaryOp, lhs: ExprId, rhs: ExprId },
    /// `operand as ty`, or `operand as% ty` if `wrapping`.
    Cast { operand: ExprId, ty: TypeExpr, wrapping: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    /// `-`, which traps on overflow.
    Neg,
    /// `-%`, which wraps on overflow.
    WrappingNeg,
    /// `!`, logical not.
    Not,
}

impl UnaryOp {
    pub fn symbol(self) -> &'static str {
        match self {
            UnaryOp::Neg => "-",
            UnaryOp::WrappingNeg => "-%",
            UnaryOp::Not => "!",
        }
    }
}

/// What kind of operation a binary operator is, which determines its operand and result types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOpKind {
    /// Integer operands, integer result.
    Arithmetic,
    /// `==` and `!=`: integer or `bool` operands, `bool` result.
    Equality,
    /// `<` and friends: integer operands, `bool` result.
    Ordering,
    /// `and` and `or`: `bool` operands and result. These short-circuit.
    Logical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    WrappingAdd,
    WrappingSub,
    WrappingMul,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl BinaryOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Rem => "%",
            BinaryOp::WrappingAdd => "+%",
            BinaryOp::WrappingSub => "-%",
            BinaryOp::WrappingMul => "*%",
            BinaryOp::Eq => "==",
            BinaryOp::Ne => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::And => "and",
            BinaryOp::Or => "or",
        }
    }

    pub fn kind(self) -> BinaryOpKind {
        match self {
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Rem
            | BinaryOp::WrappingAdd
            | BinaryOp::WrappingSub
            | BinaryOp::WrappingMul => BinaryOpKind::Arithmetic,
            BinaryOp::Eq | BinaryOp::Ne => BinaryOpKind::Equality,
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => BinaryOpKind::Ordering,
            BinaryOp::And | BinaryOp::Or => BinaryOpKind::Logical,
        }
    }
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

    pub fn stmts(&self, range: StmtRange) -> &[Stmt] {
        &self.stmts[range.start as usize..(range.start + range.len) as usize]
    }

    /// Stores a block's statements together, so the block can refer to them as one range.
    pub fn add_stmts(&mut self, stmts: Vec<Stmt>) -> StmtRange {
        let range = StmtRange { start: self.stmts.len() as u32, len: stmts.len() as u32 };
        self.stmts.extend(stmts);
        range
    }

    /// Formats the tree for `--emit=ast`, one node per line, with each node's span.
    pub fn dump(&self, sources: &SourceMap) -> String {
        let mut out = String::new();
        for decl in &self.fns {
            let name = sources.text(decl.name.span);
            dump_line(&mut out, 0, &format!("fn `{name}`"), decl.span, sources);
            self.dump_type(&mut out, 1, "return type", decl.return_type, sources);
            self.dump_expr(&mut out, 1, decl.body, sources);
        }
        out
    }

    fn dump_type(&self, out: &mut String, depth: usize, label: &str, ty: TypeExpr, sources: &SourceMap) {
        let label = format!("{label} `{}`", sources.text(ty.span()));
        dump_line(out, depth, &label, ty.span(), sources);
    }

    fn dump_expr(&self, out: &mut String, depth: usize, id: ExprId, sources: &SourceMap) {
        let expr = self.expr(id);
        match expr.kind {
            ExprKind::Integer => {
                let label = format!("integer `{}`", sources.text(expr.span));
                dump_line(out, depth, &label, expr.span, sources);
            }
            ExprKind::Name(name) => {
                let label = format!("name `{}`", sources.text(name.span));
                dump_line(out, depth, &label, expr.span, sources);
            }
            ExprKind::Block { stmts, tail } => {
                dump_line(out, depth, "block", expr.span, sources);
                for stmt in self.stmts(stmts) {
                    self.dump_stmt(out, depth + 1, *stmt, sources);
                }
                if let Some(tail) = tail {
                    self.dump_expr(out, depth + 1, tail, sources);
                }
            }
            ExprKind::Unit => dump_line(out, depth, "unit", expr.span, sources),
            ExprKind::Bool(value) => dump_line(out, depth, &format!("bool `{value}`"), expr.span, sources),
            ExprKind::If { cond, then_branch, else_branch } => {
                dump_line(out, depth, "if", expr.span, sources);
                self.dump_expr(out, depth + 1, cond, sources);
                self.dump_expr(out, depth + 1, then_branch, sources);
                if let Some(else_branch) = else_branch {
                    self.dump_expr(out, depth + 1, else_branch, sources);
                }
            }
            ExprKind::Assign { target, value } => {
                dump_line(out, depth, "assign", expr.span, sources);
                self.dump_expr(out, depth + 1, target, sources);
                self.dump_expr(out, depth + 1, value, sources);
            }
            ExprKind::Unary { op, operand } => {
                dump_line(out, depth, &format!("unary `{}`", op.symbol()), expr.span, sources);
                self.dump_expr(out, depth + 1, operand, sources);
            }
            ExprKind::Binary { op, lhs, rhs } => {
                dump_line(out, depth, &format!("binary `{}`", op.symbol()), expr.span, sources);
                self.dump_expr(out, depth + 1, lhs, sources);
                self.dump_expr(out, depth + 1, rhs, sources);
            }
            ExprKind::Cast { operand, ty, wrapping } => {
                let label = if wrapping { "cast `as%`" } else { "cast `as`" };
                dump_line(out, depth, label, expr.span, sources);
                self.dump_expr(out, depth + 1, operand, sources);
                self.dump_type(out, depth + 1, "type", ty, sources);
            }
        }
    }

    fn dump_stmt(&self, out: &mut String, depth: usize, stmt: Stmt, sources: &SourceMap) {
        match stmt {
            Stmt::Let { mutable, name, ty, init, span } => {
                let mutable = if mutable { "mut " } else { "" };
                let label = format!("let {mutable}`{}`", sources.text(name.span));
                dump_line(out, depth, &label, span, sources);
                if let Some(ty) = ty {
                    self.dump_type(out, depth + 1, "type", ty, sources);
                }
                self.dump_expr(out, depth + 1, init, sources);
            }
            Stmt::Expr { expr, semicolon } => {
                let label = if semicolon { "statement" } else { "statement (no `;`)" };
                dump_line(out, depth, label, self.expr(expr).span, sources);
                self.dump_expr(out, depth + 1, expr, sources);
            }
        }
    }
}

/// Writes one node as `<label> (<span location>)`, indented by depth.
fn dump_line(out: &mut String, depth: usize, label: &str, span: Span, sources: &SourceMap) {
    let location = sources.span_location(span);
    let indent = depth * 2;
    writeln!(out, "{:indent$}{label} ({location})", "").unwrap();
}
