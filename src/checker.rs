//! Checks that a program follows the language's rules, and builds its HIR.
//!
//! Type checking is bidirectional. Where the context requires a type, such as a variable's type
//! annotation, the expression is *checked* against that type. Otherwise its type is *inferred* from
//! its parts. See "Checker" in design/compiler.md.

use std::collections::HashMap;

use crate::ast::{self, Ast, BinaryOpKind, Ident, TypeExpr, UnaryOp};
use crate::diagnostic::{Diagnostic, Severity};
use crate::hir::{self, Hir, IntType, LocalId, Type};
use crate::intern::{Interner, Symbol};
use crate::source::{FileId, SourceMap, Span};

/// Checks a parsed file, reporting every error and warning found. Returns a HIR only if there were
/// no errors.
pub fn check(
    ast: &Ast,
    file: FileId,
    sources: &SourceMap,
    symbols: &mut Interner,
) -> (Option<Hir>, Vec<Diagnostic>) {
    let mut checker = Checker {
        ast,
        sources,
        symbols,
        hir: Hir::default(),
        diagnostics: Vec::new(),
        function: FunctionState::default(),
    };
    checker.check_file(file);

    // Warnings are found when a scope ends, after any errors inside it. Report in source order.
    let mut diagnostics = checker.diagnostics;
    diagnostics.sort_by_key(|diagnostic| diagnostic.span.start);
    let failed = diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error);
    (if failed { None } else { Some(checker.hir) }, diagnostics)
}

/// Proof that an error was reported. Functions that fail return this instead of a value, so their
/// callers can skip whatever depended on it without reporting another, redundant error.
struct Reported;

type CheckResult<T> = Result<T, Reported>;

/// What a name in scope refers to.
#[derive(Clone, Copy)]
enum Binding {
    Local(LocalId),
    /// A variable whose declaration had an error, so its type is unknown. Uses of it are skipped
    /// without reporting anything more.
    Poisoned,
}

#[derive(Clone, Copy, Default)]
struct Usage {
    read: bool,
    /// Assigned to after its declaration.
    assigned: bool,
}

/// State for the function currently being checked.
#[derive(Default)]
struct FunctionState {
    locals: Vec<hir::Local>,
    /// How each local has been used, indexed like `locals`.
    usage: Vec<Usage>,
    /// Names in scope, with the innermost scope last.
    scopes: Vec<HashMap<Symbol, Binding>>,
}

struct Checker<'a> {
    ast: &'a Ast,
    sources: &'a SourceMap,
    symbols: &'a mut Interner,
    hir: Hir,
    diagnostics: Vec<Diagnostic>,
    function: FunctionState,
}

impl Checker<'_> {
    fn error(&mut self, message: impl Into<String>, span: Span) -> Reported {
        self.diagnostics.push(Diagnostic::error(message, span));
        Reported
    }

    fn warning(&mut self, message: impl Into<String>, span: Span) {
        self.diagnostics.push(Diagnostic::warning(message, span));
    }

    fn check_file(&mut self, file: FileId) {
        let main = self.symbols.intern("main");
        let mut found_main = false;

        let ast = self.ast;
        for decl in &ast.fns {
            let name = self.symbols.intern(self.sources.text(decl.name.span));
            if name != main {
                self.error(
                    "functions other than `main` are not supported yet",
                    decl.name.span,
                );
            } else if found_main {
                self.error("`main` is defined more than once", decl.name.span);
            }
            found_main |= name == main;

            // Check every function, even ones already reported, to find errors inside them too.
            if let Ok(function) = self.check_fn(decl, name, main) {
                self.hir.fns.push(function);
            }
        }

        if !found_main {
            self.error(
                "no `main` function",
                Span {
                    file,
                    start: 0,
                    end: 0,
                },
            );
        }
    }

    fn check_fn(
        &mut self,
        decl: &ast::FnDecl,
        name: Symbol,
        main: Symbol,
    ) -> CheckResult<hir::Function> {
        self.function = FunctionState::default();
        let return_type = self.resolve_type(decl.return_type)?;
        if name == main && return_type != Type::U8 {
            self.error(
                format!("`main` must return `u8`, not `{return_type}`"),
                decl.return_type.span(),
            );
        }
        let body = self.check_expr(decl.body, return_type);
        let locals = std::mem::take(&mut self.function.locals);
        Ok(hir::Function {
            name,
            return_type,
            locals,
            body: body?,
            span: decl.span,
        })
    }

    fn resolve_type(&mut self, ty: TypeExpr) -> CheckResult<Type> {
        let name = match ty {
            TypeExpr::Unit(_) => return Ok(Type::Unit),
            TypeExpr::Name(name) => name,
        };
        let text = self.sources.text(name.span);
        if text == "bool" {
            return Ok(Type::Bool);
        }
        match int_type_from_name(text) {
            Some(Ok(int)) => Ok(Type::Int(int)),
            Some(Err(())) => {
                let message = format!(
                    "`{text}` is not supported: integer types must be 8, 16, 32, or 64 bits for now"
                );
                Err(self.error(message, name.span))
            }
            None => Err(self.error(format!("unknown type `{text}`"), name.span)),
        }
    }

    /// Checks an expression that must have type `expected`.
    fn check_expr(&mut self, id: ast::ExprId, expected: Type) -> CheckResult<hir::ExprId> {
        let expr = *self.ast.expr(id);
        let kind = match (expr.kind, expected) {
            // A literal takes its type from the context.
            (ast::ExprKind::Integer, _) => {
                let value = self.integer_value(expr.span, expr.span, expected, false)?;
                hir::ExprKind::Integer { value }
            }
            // `-` directly before a literal is part of the literal, so `-128` fits in an `i8`.
            (
                ast::ExprKind::Unary {
                    op: UnaryOp::Neg,
                    operand,
                },
                Type::Int(int),
            ) if int.signed && matches!(self.ast.expr(operand).kind, ast::ExprKind::Integer) => {
                let digits = self.ast.expr(operand).span;
                let value = self.integer_value(digits, expr.span, expected, true)?;
                hir::ExprKind::Integer { value }
            }
            (
                ast::ExprKind::Unary {
                    op: op @ (UnaryOp::Neg | UnaryOp::WrappingNeg),
                    operand,
                },
                Type::Int(int),
            ) => {
                // Check the operand even if it can't be negated, to find errors in it too, and so
                // the variables it uses count as used.
                let operand = self.check_expr(operand, expected);
                if !int.signed {
                    return Err(self.error(
                        format!("cannot negate a value of unsigned type `{int}`"),
                        expr.span,
                    ));
                }
                hir::ExprKind::Unary {
                    op,
                    operand: operand?,
                }
            }
            // Both operands of arithmetic have the result's type.
            (ast::ExprKind::Binary { op, lhs, rhs }, Type::Int(_))
                if op.kind() == BinaryOpKind::Arithmetic =>
            {
                let lhs = self.check_expr(lhs, expected);
                let rhs = self.check_expr(rhs, expected);
                hir::ExprKind::Binary {
                    op,
                    lhs: lhs?,
                    rhs: rhs?,
                }
            }
            // The expected type passes through to the block's final expression, and to both
            // branches of an `if`.
            (ast::ExprKind::Block { stmts, tail }, _) => {
                return self.check_block(expr.span, stmts, tail, Some(expected));
            }
            (
                ast::ExprKind::If {
                    cond,
                    then_branch,
                    else_branch,
                },
                _,
            ) => {
                return self.check_if(expr.span, cond, then_branch, else_branch, Some(expected));
            }
            // Anything else has a type of its own, which must match.
            _ => {
                let id = self.infer_expr(id)?;
                let found = self.hir.expr(id).ty;
                if found != expected {
                    return Err(
                        self.error(format!("expected `{expected}`, found `{found}`"), expr.span)
                    );
                }
                return Ok(id);
            }
        };
        Ok(self.hir.add_expr(hir::Expr {
            kind,
            ty: expected,
            span: expr.span,
        }))
    }

    /// Whether an expression gets its type from context, rather than having one of its own: a
    /// literal, arithmetic on only literals, or a block or `if` whose value is one of those.
    fn needs_context(&self, id: ast::ExprId) -> bool {
        match self.ast.expr(id).kind {
            ast::ExprKind::Integer => true,
            ast::ExprKind::Unary { op, operand } => {
                op != UnaryOp::Not && self.needs_context(operand)
            }
            ast::ExprKind::Binary { op, lhs, rhs } => {
                op.kind() == BinaryOpKind::Arithmetic
                    && self.needs_context(lhs)
                    && self.needs_context(rhs)
            }
            ast::ExprKind::Block {
                tail: Some(tail), ..
            } => self.needs_context(tail),
            ast::ExprKind::If {
                then_branch,
                else_branch: Some(else_branch),
                ..
            } => self.needs_context(then_branch) && self.needs_context(else_branch),
            _ => false,
        }
    }

    /// Checks an `if`. If `expected` is given, the `if` must have that type.
    fn check_if(
        &mut self,
        span: Span,
        cond: ast::ExprId,
        then_branch: ast::ExprId,
        else_branch: Option<ast::ExprId>,
        expected: Option<Type>,
    ) -> CheckResult<hir::ExprId> {
        let cond = self.check_expr(cond, Type::Bool);
        let (then_branch, else_branch, ty) = match (else_branch, expected) {
            // Without an `else`, the `if` has type `()`, so its block must too.
            (None, _) => match expected.filter(|&expected| expected != Type::Unit) {
                Some(expected) => {
                    // The block was probably meant to give the `if`'s value. Check it against that
                    // type to find any errors inside it, and report just the missing `else`.
                    let _ = self.check_expr(then_branch, expected);
                    let message = format!(
                        "expected `{expected}`, found `()`, because this `if` has no `else`"
                    );
                    return Err(self.error(message, span));
                }
                None => (self.check_expr(then_branch, Type::Unit)?, None, Type::Unit),
            },
            (Some(else_branch), Some(expected)) => {
                let then_branch = self.check_expr(then_branch, expected);
                let else_branch = self.check_expr(else_branch, expected);
                (then_branch?, Some(else_branch?), expected)
            }
            // Infer whichever branch has a type of its own, and check the other against it.
            (Some(else_branch), None) => {
                let swap = self.needs_context(then_branch) && !self.needs_context(else_branch);
                let (first, second) = if swap {
                    (else_branch, then_branch)
                } else {
                    (then_branch, else_branch)
                };
                let first = self.infer_expr(first)?;
                let ty = self.hir.expr(first).ty;
                let second = self.check_expr(second, ty)?;
                let (then_branch, else_branch) = if swap {
                    (second, first)
                } else {
                    (first, second)
                };
                (then_branch, Some(else_branch), ty)
            }
        };
        let kind = hir::ExprKind::If {
            cond: cond?,
            then_branch,
            else_branch,
        };
        Ok(self.hir.add_expr(hir::Expr { kind, ty, span }))
    }

    /// Checks an expression whose type isn't known from context, and determines it.
    fn infer_expr(&mut self, id: ast::ExprId) -> CheckResult<hir::ExprId> {
        let expr = *self.ast.expr(id);
        let (kind, ty) = match expr.kind {
            ast::ExprKind::Integer => {
                let text = self.sources.text(expr.span);
                let message = format!(
                    "cannot infer the type of integer `{text}`; it needs a type from its context, such as a \
                     type annotation"
                );
                return Err(self.error(message, expr.span));
            }
            ast::ExprKind::Name(name) => {
                let local = self.lookup(name)?;
                self.function.usage[local.0 as usize].read = true;
                (
                    hir::ExprKind::Local(local),
                    self.function.locals[local.0 as usize].ty,
                )
            }
            ast::ExprKind::Unit => (hir::ExprKind::Unit, Type::Unit),
            ast::ExprKind::Bool(value) => (hir::ExprKind::Bool(value), Type::Bool),
            ast::ExprKind::Block { stmts, tail } => {
                return self.check_block(expr.span, stmts, tail, None);
            }
            ast::ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                return self.check_if(expr.span, cond, then_branch, else_branch, None);
            }
            ast::ExprKind::Assign { target, value } => {
                (self.check_assign(target, value)?, Type::Unit)
            }
            ast::ExprKind::Unary {
                op: UnaryOp::Not,
                operand,
            } => {
                let operand = self.check_expr(operand, Type::Bool)?;
                (
                    hir::ExprKind::Unary {
                        op: UnaryOp::Not,
                        operand,
                    },
                    Type::Bool,
                )
            }
            ast::ExprKind::Unary { op, operand } => {
                let operand = self.infer_expr(operand)?;
                let ty = self.hir.expr(operand).ty;
                match ty {
                    Type::Int(int) if int.signed => (hir::ExprKind::Unary { op, operand }, ty),
                    Type::Int(int) => {
                        return Err(self.error(
                            format!("cannot negate a value of unsigned type `{int}`"),
                            expr.span,
                        ));
                    }
                    _ => {
                        return Err(
                            self.error(format!("cannot negate a value of type `{ty}`"), expr.span)
                        );
                    }
                }
            }
            // Logical operators short-circuit, but for typing they are simple: both sides are `bool`.
            ast::ExprKind::Binary { op, lhs, rhs } if op.kind() == BinaryOpKind::Logical => {
                let lhs = self.check_expr(lhs, Type::Bool);
                let rhs = self.check_expr(rhs, Type::Bool);
                (
                    hir::ExprKind::Binary {
                        op,
                        lhs: lhs?,
                        rhs: rhs?,
                    },
                    Type::Bool,
                )
            }
            ast::ExprKind::Binary { op, lhs, rhs } => {
                // Infer whichever operand has a type of its own, and check the other against it. So
                // in `1 + x` or `1 < x`, the `1` takes its type from `x`.
                let swap = self.needs_context(lhs) && !self.needs_context(rhs);
                let (first, second) = if swap { (rhs, lhs) } else { (lhs, rhs) };
                let first = self.infer_expr(first)?;
                let ty = self.hir.expr(first).ty;
                let allowed = match op.kind() {
                    BinaryOpKind::Equality => matches!(ty, Type::Int(_) | Type::Bool),
                    _ => matches!(ty, Type::Int(_)),
                };
                if !allowed {
                    let message = format!(
                        "operator `{}` cannot be applied to type `{ty}`",
                        op.symbol()
                    );
                    return Err(self.error(message, expr.span));
                }
                let second = self.check_expr(second, ty)?;
                let (lhs, rhs) = if swap {
                    (second, first)
                } else {
                    (first, second)
                };
                let result = if op.kind() == BinaryOpKind::Arithmetic {
                    ty
                } else {
                    Type::Bool
                };
                (hir::ExprKind::Binary { op, lhs, rhs }, result)
            }
            ast::ExprKind::Cast {
                operand,
                ty,
                wrapping,
            } => {
                let target = self.resolve_type(ty)?;
                // A literal being converted takes the target type, so `300 as u8` is an error
                // rather than a trap.
                let operand = if self.needs_context(operand) {
                    self.check_expr(operand, target)?
                } else {
                    self.infer_expr(operand)?
                };
                let from = self.hir.expr(operand).ty;
                if !matches!((from, target), (Type::Int(_), Type::Int(_))) {
                    let message = format!(
                        "cannot convert `{from}` to `{target}`: only integers can be converted"
                    );
                    return Err(self.error(message, expr.span));
                }
                (hir::ExprKind::Cast { operand, wrapping }, target)
            }
        };
        Ok(self.hir.add_expr(hir::Expr {
            kind,
            ty,
            span: expr.span,
        }))
    }

    /// Checks a block. If `expected` is given, the block must have that type.
    fn check_block(
        &mut self,
        span: Span,
        stmts: ast::StmtRange,
        tail: Option<ast::ExprId>,
        expected: Option<Type>,
    ) -> CheckResult<hir::ExprId> {
        self.function.scopes.push(HashMap::new());
        let mut hir_stmts = Vec::new();
        let ast = self.ast;
        for stmt in ast.stmts(stmts) {
            // A failed statement doesn't stop the rest of the block from being checked.
            if let Ok(stmt) = self.check_stmt(*stmt) {
                hir_stmts.push(stmt);
            }
        }
        let tail = tail.map(|tail| match expected {
            Some(expected) => self.check_expr(tail, expected),
            None => self.infer_expr(tail),
        });
        self.end_scope();

        let (tail, ty) = match tail {
            Some(tail) => {
                let tail = tail?;
                (Some(tail), self.hir.expr(tail).ty)
            }
            None => match expected {
                Some(expected) if expected != Type::Unit => {
                    let message = format!(
                        "expected `{expected}`, found `()`, because the block has no final expression"
                    );
                    return Err(self.error(message, span));
                }
                _ => (None, Type::Unit),
            },
        };
        let stmts = self.hir.add_stmts(hir_stmts);
        Ok(self.hir.add_expr(hir::Expr {
            kind: hir::ExprKind::Block { stmts, tail },
            ty,
            span,
        }))
    }

    /// Leaves the innermost scope, warning about any of its variables that weren't used.
    fn end_scope(&mut self) {
        let scope = self.function.scopes.pop().expect("no scope to end");
        for binding in scope.into_values() {
            let Binding::Local(local) = binding else {
                continue;
            };
            let decl = self.function.locals[local.0 as usize];
            let usage = self.function.usage[local.0 as usize];
            let name = self.symbols.get(decl.name).to_string();
            if !usage.read {
                self.warning(format!("unused variable `{name}`"), decl.span);
            }
            if decl.mutable && !usage.assigned {
                self.warning(
                    format!("variable `{name}` is declared `mut` but never assigned to"),
                    decl.span,
                );
            }
        }
    }

    fn check_stmt(&mut self, stmt: ast::Stmt) -> CheckResult<hir::Stmt> {
        match stmt {
            ast::Stmt::Let {
                mutable,
                name,
                ty,
                init,
                ..
            } => self.check_let(mutable, name, ty, init),
            ast::Stmt::Expr { expr, .. } => {
                let id = self.infer_expr(expr)?;
                let ty = self.hir.expr(id).ty;
                if ty != Type::Unit {
                    return Err(self.error(
                        format!("unused value of type `{ty}`"),
                        self.ast.expr(expr).span,
                    ));
                }
                Ok(hir::Stmt::Expr(id))
            }
        }
    }

    fn check_let(
        &mut self,
        mutable: bool,
        name: Ident,
        annotation: Option<TypeExpr>,
        init: ast::ExprId,
    ) -> CheckResult<hir::Stmt> {
        // The variable isn't in scope in its own initializer, so check that first.
        let (ty, init) = match annotation {
            Some(annotation) => match self.resolve_type(annotation) {
                Ok(ty) => (Some(ty), self.check_expr(init, ty)),
                Err(reported) => (None, Err(reported)),
            },
            None => {
                let init = self.infer_expr(init);
                let ty = init.as_ref().ok().map(|&init| self.hir.expr(init).ty);
                (ty, init)
            }
        };

        let symbol = self.symbols.intern(self.sources.text(name.span));
        if self.find_binding(symbol).is_some() {
            let message = format!(
                "`{}` is already defined; shadowing is not allowed",
                self.symbols.get(symbol)
            );
            return Err(self.error(message, name.span));
        }

        let binding = match ty {
            Some(ty) => {
                let local = LocalId(self.function.locals.len() as u32);
                self.function.locals.push(hir::Local {
                    name: symbol,
                    ty,
                    mutable,
                    span: name.span,
                });
                self.function.usage.push(Usage::default());
                Binding::Local(local)
            }
            None => Binding::Poisoned,
        };
        self.function
            .scopes
            .last_mut()
            .expect("let outside a scope")
            .insert(symbol, binding);

        match binding {
            Binding::Local(local) => Ok(hir::Stmt::Let { local, init: init? }),
            // The type is only unknown if checking the annotation or initializer reported an error.
            Binding::Poisoned => Err(Reported),
        }
    }

    fn check_assign(
        &mut self,
        target: ast::ExprId,
        value: ast::ExprId,
    ) -> CheckResult<hir::ExprKind> {
        let target_expr = *self.ast.expr(target);
        let ast::ExprKind::Name(name) = target_expr.kind else {
            return Err(self.error("only variables can be assigned to", target_expr.span));
        };
        let local = self.lookup(name)?;
        let decl = self.function.locals[local.0 as usize];
        self.function.usage[local.0 as usize].assigned = true;

        // Check the value even if the variable can't be assigned to, to find errors in it too.
        let value = self.check_expr(value, decl.ty);
        if !decl.mutable {
            let message = format!(
                "cannot assign to `{}`, because it is not declared `mut`",
                self.symbols.get(decl.name)
            );
            return Err(self.error(message, target_expr.span));
        }

        let target = self.hir.add_expr(hir::Expr {
            kind: hir::ExprKind::Local(local),
            ty: decl.ty,
            span: target_expr.span,
        });
        Ok(hir::ExprKind::Assign {
            target,
            value: value?,
        })
    }

    /// Finds what a name refers to in the current scopes, if anything.
    fn find_binding(&self, symbol: Symbol) -> Option<Binding> {
        self.function
            .scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&symbol).copied())
    }

    /// Resolves a variable name, reporting an error if it isn't defined.
    fn lookup(&mut self, name: Ident) -> CheckResult<LocalId> {
        let text = self.sources.text(name.span);
        let symbol = self.symbols.intern(text);
        match self.find_binding(symbol) {
            Some(Binding::Local(local)) => Ok(local),
            // The declaration's error was already reported.
            Some(Binding::Poisoned) => Err(Reported),
            None => Err(self.error(format!("undefined variable `{text}`"), name.span)),
        }
    }

    /// Parses an integer literal's value, negated if `negative`, and checks that it fits in `ty`.
    /// `digits` is the literal itself, and `span` is the whole expression, including any `-`.
    fn integer_value(
        &mut self,
        digits: Span,
        span: Span,
        ty: Type,
        negative: bool,
    ) -> CheckResult<i128> {
        let text = format!(
            "{}{}",
            if negative { "-" } else { "" },
            self.sources.text(digits)
        );
        let Type::Int(int) = ty else {
            return Err(self.error(format!("expected `{ty}`, found integer `{text}`"), span));
        };
        // The lexer guarantees the literal is only decimal digits. Overflowing an `i128` means it's
        // too large for any type.
        let magnitude = self
            .sources
            .text(digits)
            .bytes()
            .try_fold(0i128, |value, digit| {
                value.checked_mul(10)?.checked_add((digit - b'0').into())
            });
        let value = magnitude.map(|magnitude| if negative { -magnitude } else { magnitude });
        match value {
            Some(value) if int.min() <= value && value <= int.max() => Ok(value),
            _ => {
                let message = format!(
                    "`{text}` does not fit in `{ty}` (range {} to {})",
                    int.min(),
                    int.max()
                );
                Err(self.error(message, span))
            }
        }
    }
}

/// Parses an integer type name like `u8` or `i32`. Returns `None` if the name isn't an integer
/// type at all, and `Some(Err(()))` for an integer type with an unsupported width, like `u7`.
fn int_type_from_name(name: &str) -> Option<Result<IntType, ()>> {
    let signed = match name.as_bytes().first()? {
        b'u' => false,
        b'i' => true,
        _ => return None,
    };
    let bits = &name[1..];
    if bits.is_empty() || !bits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match bits {
        "8" | "16" | "32" | "64" => Some(Ok(IntType {
            signed,
            bits: bits.parse().unwrap(),
        })),
        _ => Some(Err(())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer, parser};

    fn check_str(text: &str) -> (Option<Hir>, Vec<Diagnostic>, Interner) {
        let mut sources = SourceMap::default();
        let file = sources.add("test.jo".into(), text.into()).unwrap();
        let (tokens, diagnostics) = lexer::lex(file, text);
        assert!(
            diagnostics.is_empty(),
            "unexpected lexer diagnostics: {diagnostics:?}"
        );
        let ast = parser::parse(&tokens, text).expect("unexpected parse error");
        let mut symbols = Interner::default();
        let (hir, diagnostics) = check(&ast, file, &sources, &mut symbols);
        (hir, diagnostics, symbols)
    }

    /// Checks `text` and returns its diagnostics, as `error: <message>` or `warning: <message>`.
    fn diagnostics(text: &str) -> Vec<String> {
        let (hir, diagnostics, _) = check_str(text);
        let failed = diagnostics.iter().any(|d| d.severity == Severity::Error);
        assert_eq!(
            hir.is_some(),
            !failed,
            "a HIR should exist exactly when there are no errors"
        );
        diagnostics
            .into_iter()
            .map(|d| {
                let severity = match d.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                };
                format!("{severity}: {}", d.message)
            })
            .collect()
    }

    /// Wraps statements in a `main` that returns 0.
    fn in_main(body: &str) -> String {
        format!("fn main() -> u8 {{ {body} 0 }}")
    }

    #[test]
    fn minimal_program() {
        let (hir, diagnostics, symbols) = check_str("fn main() -> u8 { 42 }");
        assert!(diagnostics.is_empty());
        let hir = hir.unwrap();
        assert_eq!(hir.fns.len(), 1);
        let main = &hir.fns[0];
        assert_eq!(symbols.get(main.name), "main");
        assert_eq!(main.return_type, Type::U8);

        let body = hir.expr(main.body);
        assert_eq!(body.ty, Type::U8);
        let hir::ExprKind::Block {
            tail: Some(tail), ..
        } = body.kind
        else {
            panic!("unexpected body: {body:?}")
        };
        let tail = hir.expr(tail);
        assert_eq!(tail.ty, Type::U8);
        assert!(matches!(tail.kind, hir::ExprKind::Integer { value: 42 }));
    }

    #[test]
    fn integer_range() {
        assert!(diagnostics("fn main() -> u8 { 0 }").is_empty());
        assert!(diagnostics("fn main() -> u8 { 255 }").is_empty());
        assert!(diagnostics("fn main() -> u8 { 000255 }").is_empty());
        assert_eq!(
            diagnostics("fn main() -> u8 { 256 }"),
            ["error: `256` does not fit in `u8` (range 0 to 255)"]
        );
        // Larger than a u128.
        let huge = "9".repeat(50);
        assert_eq!(
            diagnostics(&format!("fn main() -> u8 {{ {huge} }}")),
            [format!(
                "error: `{huge}` does not fit in `u8` (range 0 to 255)"
            )]
        );
    }

    #[test]
    fn program_rules() {
        assert_eq!(diagnostics(""), ["error: no `main` function"]);
        assert_eq!(
            diagnostics("fn main() -> Foo { 0 }"),
            ["error: unknown type `Foo`"]
        );
        assert_eq!(
            diagnostics("fn main() -> i32 { 0 }"),
            ["error: `main` must return `u8`, not `i32`"]
        );
        assert_eq!(
            diagnostics("fn main() -> () { }"),
            ["error: `main` must return `u8`, not `()`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { 0 } fn main() -> u8 { 0 }"),
            ["error: `main` is defined more than once"]
        );
        // "No `main`" points at the start of the file, so it comes first.
        assert_eq!(
            diagnostics("fn other() -> u8 { 0 }"),
            [
                "error: no `main` function",
                "error: functions other than `main` are not supported yet"
            ]
        );
    }

    #[test]
    fn reports_every_error() {
        assert_eq!(
            diagnostics("fn main() -> Foo { 0 } fn other() -> u8 { 300 }"),
            [
                "error: unknown type `Foo`",
                "error: functions other than `main` are not supported yet",
                "error: `300` does not fit in `u8` (range 0 to 255)",
            ]
        );
    }

    #[test]
    fn variables() {
        let text = "fn main() -> u8 { let x: u8 = 1; let y = x; let mut z: u8 = 0; z = y; z }";
        assert!(diagnostics(text).is_empty());

        // `y` gets its type from `x`.
        let (hir, _, symbols) = check_str(text);
        let main = &hir.unwrap().fns[0];
        let types: Vec<String> = main
            .locals
            .iter()
            .map(|local| format!("{}: {}", symbols.get(local.name), local.ty))
            .collect();
        assert_eq!(types, ["x: u8", "y: u8", "z: u8"]);
    }

    #[test]
    fn blocks_and_unit() {
        assert!(diagnostics("fn main() -> u8 { let x: u8 = { let y: u8 = 1; y }; x }").is_empty());
        assert!(
            diagnostics("fn main() -> u8 { let mut x: u8 = 0; { x = 1; } { x = 2 } x }").is_empty()
        );
        // Assignment has type `()`, so it can be stored in a `()` variable, and that variable can be
        // a statement.
        assert!(
            diagnostics("fn main() -> u8 { let mut x: u8 = 0; let unit: () = x = 1; unit; x }")
                .is_empty()
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { }"),
            ["error: expected `u8`, found `()`, because the block has no final expression"]
        );
    }

    #[test]
    fn type_errors() {
        assert_eq!(
            diagnostics(&in_main("let x = 5;")),
            [
                "error: cannot infer the type of integer `5`; it needs a type from its context, such as a type annotation"
            ]
        );
        assert_eq!(
            diagnostics(&in_main("let x: () = 5; x;")),
            ["error: expected `()`, found integer `5`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let mut x: u8 = 0; x = 1 }"),
            [
                "warning: unused variable `x`",
                "error: expected `u8`, found `()`"
            ]
        );
        // Assignment is `()`, so chaining assignments is a type error.
        assert_eq!(
            diagnostics(&in_main(
                "let mut x: u8 = 0; let mut y: u8 = 0; x = y = 5; x; y;"
            )),
            [
                "error: expected `u8`, found `()`",
                "error: unused value of type `u8`",
                "error: unused value of type `u8`"
            ]
        );
    }

    #[test]
    fn statement_errors() {
        assert_eq!(
            diagnostics(&in_main("let x: u8 = 1; x;")),
            ["error: unused value of type `u8`"]
        );
        assert_eq!(
            diagnostics(&in_main("let x: u8 = 1; { x }")),
            ["error: unused value of type `u8`"]
        );
    }

    #[test]
    fn name_errors() {
        assert_eq!(
            diagnostics("fn main() -> u8 { x }"),
            ["error: undefined variable `x`"]
        );
        // Diagnostics are in source order: the declared name comes before its initializer.
        assert_eq!(
            diagnostics(&in_main("let x: u8 = x;")),
            [
                "warning: unused variable `x`",
                "error: undefined variable `x`"
            ]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { { let x: u8 = 1; } x }"),
            [
                "warning: unused variable `x`",
                "error: undefined variable `x`"
            ]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let x: u8 = 1; let x: u8 = 2; x }"),
            ["error: `x` is already defined; shadowing is not allowed"]
        );
        // The rejected inner `x` isn't declared, so the final `x` refers to the outer one.
        assert_eq!(
            diagnostics("fn main() -> u8 { let x: u8 = 1; { let x: u8 = 2; x } }"),
            ["error: `x` is already defined; shadowing is not allowed"]
        );
        // After a scope ends, its names can be reused.
        assert!(
            diagnostics("fn main() -> u8 { { let mut x: u8 = 1; x = x; } let x: u8 = 2; x }")
                .is_empty()
        );
    }

    #[test]
    fn failed_declarations_do_not_cascade() {
        // `x` has no type, but using it afterward reports nothing more.
        assert_eq!(
            diagnostics("fn main() -> u8 { let x = 5; x }"),
            [
                "error: cannot infer the type of integer `5`; it needs a type from its context, such as a type annotation"
            ]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let x: Foo = 5; x }"),
            ["error: unknown type `Foo`"]
        );
    }

    #[test]
    fn assignment_errors() {
        assert_eq!(
            diagnostics(&in_main("let x: u8 = 1; x = 2; x;")),
            [
                "error: cannot assign to `x`, because it is not declared `mut`",
                "error: unused value of type `u8`"
            ]
        );
        assert_eq!(
            diagnostics(&in_main("5 = 3;")),
            ["error: only variables can be assigned to"]
        );
        assert_eq!(
            diagnostics(&in_main("y = 3;")),
            ["error: undefined variable `y`"]
        );
        // The value is still checked when the variable can't be assigned to.
        assert_eq!(
            diagnostics("fn main() -> u8 { let x: u8 = 1; x = 300; x }"),
            [
                "error: cannot assign to `x`, because it is not declared `mut`",
                "error: `300` does not fit in `u8` (range 0 to 255)",
            ]
        );
    }

    #[test]
    fn integer_types() {
        assert!(
            diagnostics("fn main() -> u8 { let a: i8 = -128; let b: i8 = 127; (a +% b) as% u8 }")
                .is_empty()
        );
        assert!(
            diagnostics("fn main() -> u8 { let a: u64 = 18446744073709551615; a as% u8 }")
                .is_empty()
        );
        assert!(
            diagnostics("fn main() -> u8 { let a: i64 = -9223372036854775808; a as% u8 }")
                .is_empty()
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let a: i8 = -129; a as% u8 }"),
            ["error: `-129` does not fit in `i8` (range -128 to 127)"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let a: i8 = 128; a as% u8 }"),
            ["error: `128` does not fit in `i8` (range -128 to 127)"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let a: u7 = 1; a as% u8 }"),
            ["error: `u7` is not supported: integer types must be 8, 16, 32, or 64 bits for now"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let a: i = 1; a as% u8 }"),
            ["error: unknown type `i`"]
        );
    }

    #[test]
    fn arithmetic() {
        assert!(diagnostics("fn main() -> u8 { let x: u8 = 1; x + 2 }").is_empty());
        // A literal on the left takes its type from the right.
        assert!(diagnostics("fn main() -> u8 { let x: u8 = 1; let y = 2 + x; y }").is_empty());
        assert!(diagnostics("fn main() -> u8 { (1 + 2) * 3 / 4 % 5 -% 6 }").is_empty());
        assert!(diagnostics("fn main() -> u8 { let x: u16 = 300; (x * 2 - 1) as u8 }").is_empty());
        assert!(
            diagnostics("fn main() -> u8 { let x: i32 = 5; (-x + 10 -% -%x) as u8 }").is_empty()
        );

        assert_eq!(
            diagnostics("fn main() -> u8 { let a: u8 = 1; let b: u16 = 2; a + b }"),
            ["error: expected `u8`, found `u16`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let a: u8 = 1; let b: u16 = 2; let c = a + b; c }"),
            ["error: expected `u8`, found `u16`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let x = 1 + 2; x }"),
            [
                "error: cannot infer the type of integer `1`; it needs a type from its context, such as a type annotation"
            ]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let u: () = (); let v = u + u; v; 0 }"),
            ["error: operator `+` cannot be applied to type `()`"]
        );
    }

    #[test]
    fn negation() {
        assert_eq!(
            diagnostics("fn main() -> u8 { let x: u8 = 1; -x }"),
            ["error: cannot negate a value of unsigned type `u8`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let x: u8 = 1; let y = -%x; y }"),
            ["error: cannot negate a value of unsigned type `u8`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { -1 }"),
            ["error: cannot negate a value of unsigned type `u8`"]
        );
    }

    #[test]
    fn conversions() {
        assert!(diagnostics("fn main() -> u8 { let x: i64 = -1; x as% u8 }").is_empty());
        // A literal takes the target type, so an out-of-range literal is a compile error.
        assert_eq!(
            diagnostics("fn main() -> u8 { 300 as u8 }"),
            ["error: `300` does not fit in `u8` (range 0 to 255)"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { () as u8 }"),
            ["error: cannot convert `()` to `u8`: only integers can be converted"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let x: u8 = 1; let y: () = x as (); y; 0 }"),
            ["error: cannot convert `u8` to `()`: only integers can be converted"]
        );
    }

    #[test]
    fn booleans_and_comparisons() {
        assert!(
            diagnostics(
                "fn main() -> u8 { let a = true; let b = !a or a and false; if b { 1 } else { 0 } }"
            )
            .is_empty()
        );
        // A literal on the left of a comparison takes the type of the right.
        assert!(
            diagnostics("fn main() -> u8 { let x: u8 = 3; if 2 < x { 1 } else { 0 } }").is_empty()
        );
        assert!(
            diagnostics("fn main() -> u8 { let a = true; if a == false { 1 } else { 0 } }")
                .is_empty()
        );
        assert!(
            diagnostics(
                "fn main() -> u8 { let x: i64 = -1; if x != 0 and x >= -5 { 1 } else { 0 } }"
            )
            .is_empty()
        );

        assert_eq!(
            diagnostics("fn main() -> u8 { let a = true; if a < true { 1 } else { 0 } }"),
            ["error: operator `<` cannot be applied to type `bool`"]
        );
        assert_eq!(
            diagnostics(
                "fn main() -> u8 { let x: u8 = 1; let y: u16 = 2; if x == y { 1 } else { 0 } }"
            ),
            ["error: expected `u8`, found `u16`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let a = true; let b = a + a; b; 0 }"),
            ["error: operator `+` cannot be applied to type `bool`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let x: u8 = 1; if !x { 1 } else { 0 } }"),
            ["error: expected `bool`, found `u8`"]
        );
        assert_eq!(
            diagnostics(
                "fn main() -> u8 { let x: u8 = 1; let c = x and true; if c { 1 } else { 0 } }"
            ),
            ["error: expected `bool`, found `u8`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let b = true; let c = -b; c; 0 }"),
            ["error: cannot negate a value of type `bool`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { true as u8 }"),
            ["error: cannot convert `bool` to `u8`: only integers can be converted"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let b: bool = 1; if b { 1 } else { 0 } }"),
            ["error: expected `bool`, found integer `1`"]
        );
    }

    #[test]
    fn if_expressions() {
        assert!(
            diagnostics("fn main() -> u8 { let c = true; if c { 1 } else if !c { 2 } else { 3 } }")
                .is_empty()
        );
        assert!(
            diagnostics("fn main() -> u8 { let mut x: u8 = 0; let c = true; if c { x = 1 } x }")
                .is_empty()
        );
        // Without an expected type, a literal branch takes the type of the other branch.
        assert!(
            diagnostics(
                "fn main() -> u8 { let c = true; let x: u8 = 1; let y = if c { 2 } else { x }; y }"
            )
            .is_empty()
        );

        assert_eq!(
            diagnostics("fn main() -> u8 { let x: u8 = 1; if x { 1 } else { 2 } }"),
            ["error: expected `bool`, found `u8`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { if true { 1 } else { () } }"),
            ["error: expected `u8`, found `()`"]
        );
        assert_eq!(
            diagnostics(
                "fn main() -> u8 { let a: u8 = 1; let b: u16 = 2; let c = true; let d = if c { a } else { b }; d }"
            ),
            ["error: expected `u8`, found `u16`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { if true { 1 } }"),
            ["error: expected `u8`, found `()`, because this `if` has no `else`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let c = true; if c { 5 } 0 }"),
            ["error: expected `()`, found integer `5`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let c = true; let x = if c { 1 } else { 2 }; x }"),
            [
                "error: cannot infer the type of integer `1`; it needs a type from its context, such as a type annotation"
            ]
        );
        // Errors in the condition and both branches are all reported.
        assert_eq!(
            diagnostics("fn main() -> u8 { if 1 { 256 } else { -1 } }"),
            [
                "error: expected `bool`, found integer `1`",
                "error: `256` does not fit in `u8` (range 0 to 255)",
                "error: cannot negate a value of unsigned type `u8`",
            ]
        );
    }

    #[test]
    fn warnings() {
        assert_eq!(
            diagnostics(&in_main("let x: u8 = 1;")),
            ["warning: unused variable `x`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let mut x: u8 = 1; x }"),
            ["warning: variable `x` is declared `mut` but never assigned to"]
        );
        assert_eq!(
            diagnostics(&in_main("let mut x: u8 = 1;")),
            [
                "warning: unused variable `x`",
                "warning: variable `x` is declared `mut` but never assigned to"
            ]
        );
        // Assigning to a variable doesn't count as reading it.
        assert_eq!(
            diagnostics(&in_main("let mut x: u8 = 1; x = 2;")),
            ["warning: unused variable `x`"]
        );
        // A program with only warnings still compiles.
        assert!(check_str(&in_main("let x: u8 = 1;")).0.is_some());
    }
}
