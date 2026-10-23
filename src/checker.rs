//! Checks that a program follows the language's rules, and builds its HIR.
//!
//! Type checking is bidirectional. Where the context requires a type, such as a variable's type
//! annotation, the expression is *checked* against that type. Otherwise its type is *inferred* from
//! its parts. See "Checker" in design/compiler.md.

use std::collections::HashMap;

use crate::ast::{self, Ast, Ident, TypeExpr};
use crate::diagnostic::{Diagnostic, Severity};
use crate::hir::{self, Hir, LocalId, Type};
use crate::intern::{Interner, Symbol};
use crate::source::{FileId, SourceMap, Span};

/// Checks a parsed file, reporting every error and warning found. Returns a HIR only if there were
/// no errors.
pub fn check(ast: &Ast, file: FileId, sources: &SourceMap, symbols: &mut Interner) -> (Option<Hir>, Vec<Diagnostic>) {
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
    let failed = diagnostics.iter().any(|diagnostic| diagnostic.severity == Severity::Error);
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
                self.error("functions other than `main` are not supported yet", decl.name.span);
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
            self.error("no `main` function", Span { file, start: 0, end: 0 });
        }
    }

    fn check_fn(&mut self, decl: &ast::FnDecl, name: Symbol, main: Symbol) -> CheckResult<hir::Function> {
        self.function = FunctionState::default();
        let return_type = self.resolve_type(decl.return_type)?;
        if name == main && return_type != Type::U8 {
            self.error(format!("`main` must return `u8`, not `{return_type}`"), decl.return_type.span());
        }
        let body = self.check_expr(decl.body, return_type);
        let locals = std::mem::take(&mut self.function.locals);
        Ok(hir::Function { name, return_type, locals, body: body?, span: decl.span })
    }

    fn resolve_type(&mut self, ty: TypeExpr) -> CheckResult<Type> {
        match ty {
            TypeExpr::Unit(_) => Ok(Type::Unit),
            TypeExpr::Name(name) => match self.sources.text(name.span) {
                "u8" => Ok(Type::U8),
                text => Err(self.error(format!("unknown type `{text}`"), name.span)),
            },
        }
    }

    /// Checks an expression that must have type `expected`.
    fn check_expr(&mut self, id: ast::ExprId, expected: Type) -> CheckResult<hir::ExprId> {
        let expr = *self.ast.expr(id);
        match expr.kind {
            // A literal takes its type from the context.
            ast::ExprKind::Integer => {
                let value = self.integer_value(expr.span, expected)?;
                let kind = hir::ExprKind::Integer { value };
                Ok(self.hir.add_expr(hir::Expr { kind, ty: expected, span: expr.span }))
            }
            // The expected type passes through to the block's final expression.
            ast::ExprKind::Block { stmts, tail } => self.check_block(expr.span, stmts, tail, Some(expected)),
            ast::ExprKind::Name(_) | ast::ExprKind::Assign { .. } => {
                let id = self.infer_expr(id)?;
                let found = self.hir.expr(id).ty;
                if found != expected {
                    return Err(self.error(format!("expected `{expected}`, found `{found}`"), expr.span));
                }
                Ok(id)
            }
        }
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
                (hir::ExprKind::Local(local), self.function.locals[local.0 as usize].ty)
            }
            ast::ExprKind::Block { stmts, tail } => return self.check_block(expr.span, stmts, tail, None),
            ast::ExprKind::Assign { target, value } => (self.check_assign(target, value)?, Type::Unit),
        };
        Ok(self.hir.add_expr(hir::Expr { kind, ty, span: expr.span }))
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
                    let message = format!("expected `{expected}`, found `()`, because the block has no final expression");
                    return Err(self.error(message, span));
                }
                _ => (None, Type::Unit),
            },
        };
        let stmts = self.hir.add_stmts(hir_stmts);
        Ok(self.hir.add_expr(hir::Expr { kind: hir::ExprKind::Block { stmts, tail }, ty, span }))
    }

    /// Leaves the innermost scope, warning about any of its variables that weren't used.
    fn end_scope(&mut self) {
        let scope = self.function.scopes.pop().expect("no scope to end");
        for binding in scope.into_values() {
            let Binding::Local(local) = binding else { continue };
            let decl = self.function.locals[local.0 as usize];
            let usage = self.function.usage[local.0 as usize];
            let name = self.symbols.get(decl.name).to_string();
            if !usage.read {
                self.warning(format!("unused variable `{name}`"), decl.span);
            }
            if decl.mutable && !usage.assigned {
                self.warning(format!("variable `{name}` is declared `mut` but never assigned to"), decl.span);
            }
        }
    }

    fn check_stmt(&mut self, stmt: ast::Stmt) -> CheckResult<hir::Stmt> {
        match stmt {
            ast::Stmt::Let { mutable, name, ty, init, .. } => self.check_let(mutable, name, ty, init),
            ast::Stmt::Expr { expr, .. } => {
                let id = self.infer_expr(expr)?;
                let ty = self.hir.expr(id).ty;
                if ty != Type::Unit {
                    return Err(self.error(format!("unused value of type `{ty}`"), self.ast.expr(expr).span));
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
            let message = format!("`{}` is already defined; shadowing is not allowed", self.symbols.get(symbol));
            return Err(self.error(message, name.span));
        }

        let binding = match ty {
            Some(ty) => {
                let local = LocalId(self.function.locals.len() as u32);
                self.function.locals.push(hir::Local { name: symbol, ty, mutable, span: name.span });
                self.function.usage.push(Usage::default());
                Binding::Local(local)
            }
            None => Binding::Poisoned,
        };
        self.function.scopes.last_mut().expect("let outside a scope").insert(symbol, binding);

        match binding {
            Binding::Local(local) => Ok(hir::Stmt::Let { local, init: init? }),
            // The type is only unknown if checking the annotation or initializer reported an error.
            Binding::Poisoned => Err(Reported),
        }
    }

    fn check_assign(&mut self, target: ast::ExprId, value: ast::ExprId) -> CheckResult<hir::ExprKind> {
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

        let target = self.hir.add_expr(hir::Expr { kind: hir::ExprKind::Local(local), ty: decl.ty, span: target_expr.span });
        Ok(hir::ExprKind::Assign { target, value: value? })
    }

    /// Finds what a name refers to in the current scopes, if anything.
    fn find_binding(&self, symbol: Symbol) -> Option<Binding> {
        self.function.scopes.iter().rev().find_map(|scope| scope.get(&symbol).copied())
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

    /// Parses an integer literal's value, and checks that it fits in `ty`.
    fn integer_value(&mut self, span: Span, ty: Type) -> CheckResult<u128> {
        let text = self.sources.text(span);
        let Some(max) = ty.max_value() else {
            return Err(self.error(format!("expected `{ty}`, found integer `{text}`"), span));
        };
        // The lexer guarantees the literal is only decimal digits. Overflowing a `u128` means it's
        // too large for any type.
        let value = text
            .bytes()
            .try_fold(0u128, |value, digit| value.checked_mul(10)?.checked_add((digit - b'0').into()));
        match value {
            Some(value) if value <= max => Ok(value),
            _ => Err(self.error(format!("`{text}` does not fit in `{ty}` (range 0 to {max})"), span)),
        }
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
        assert!(diagnostics.is_empty(), "unexpected lexer diagnostics: {diagnostics:?}");
        let ast = parser::parse(&tokens, text).expect("unexpected parse error");
        let mut symbols = Interner::default();
        let (hir, diagnostics) = check(&ast, file, &sources, &mut symbols);
        (hir, diagnostics, symbols)
    }

    /// Checks `text` and returns its diagnostics, as `error: <message>` or `warning: <message>`.
    fn diagnostics(text: &str) -> Vec<String> {
        let (hir, diagnostics, _) = check_str(text);
        let failed = diagnostics.iter().any(|d| d.severity == Severity::Error);
        assert_eq!(hir.is_some(), !failed, "a HIR should exist exactly when there are no errors");
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
        let hir::ExprKind::Block { tail: Some(tail), .. } = body.kind else { panic!("unexpected body: {body:?}") };
        let tail = hir.expr(tail);
        assert_eq!(tail.ty, Type::U8);
        assert!(matches!(tail.kind, hir::ExprKind::Integer { value: 42 }));
    }

    #[test]
    fn integer_range() {
        assert!(diagnostics("fn main() -> u8 { 0 }").is_empty());
        assert!(diagnostics("fn main() -> u8 { 255 }").is_empty());
        assert!(diagnostics("fn main() -> u8 { 000255 }").is_empty());
        assert_eq!(diagnostics("fn main() -> u8 { 256 }"), ["error: `256` does not fit in `u8` (range 0 to 255)"]);
        // Larger than a u128.
        let huge = "9".repeat(50);
        assert_eq!(
            diagnostics(&format!("fn main() -> u8 {{ {huge} }}")),
            [format!("error: `{huge}` does not fit in `u8` (range 0 to 255)")]
        );
    }

    #[test]
    fn program_rules() {
        assert_eq!(diagnostics(""), ["error: no `main` function"]);
        assert_eq!(diagnostics("fn main() -> i32 { 0 }"), ["error: unknown type `i32`"]);
        assert_eq!(diagnostics("fn main() -> () { }"), ["error: `main` must return `u8`, not `()`"]);
        assert_eq!(
            diagnostics("fn main() -> u8 { 0 } fn main() -> u8 { 0 }"),
            ["error: `main` is defined more than once"]
        );
        // "No `main`" points at the start of the file, so it comes first.
        assert_eq!(
            diagnostics("fn other() -> u8 { 0 }"),
            ["error: no `main` function", "error: functions other than `main` are not supported yet"]
        );
    }

    #[test]
    fn reports_every_error() {
        assert_eq!(
            diagnostics("fn main() -> i32 { 0 } fn other() -> u8 { 300 }"),
            [
                "error: unknown type `i32`",
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
        let types: Vec<String> =
            main.locals.iter().map(|local| format!("{}: {}", symbols.get(local.name), local.ty)).collect();
        assert_eq!(types, ["x: u8", "y: u8", "z: u8"]);
    }

    #[test]
    fn blocks_and_unit() {
        assert!(diagnostics("fn main() -> u8 { let x: u8 = { let y: u8 = 1; y }; x }").is_empty());
        assert!(diagnostics("fn main() -> u8 { let mut x: u8 = 0; { x = 1; } { x = 2 } x }").is_empty());
        // Assignment has type `()`, so it can be stored in a `()` variable, and that variable can be
        // a statement.
        assert!(diagnostics("fn main() -> u8 { let mut x: u8 = 0; let unit: () = x = 1; unit; x }").is_empty());
        assert_eq!(
            diagnostics("fn main() -> u8 { }"),
            ["error: expected `u8`, found `()`, because the block has no final expression"]
        );
    }

    #[test]
    fn type_errors() {
        assert_eq!(
            diagnostics(&in_main("let x = 5;")),
            ["error: cannot infer the type of integer `5`; it needs a type from its context, such as a type annotation"]
        );
        assert_eq!(
            diagnostics(&in_main("let x: () = 5; x;")),
            ["error: expected `()`, found integer `5`"]
        );
        assert_eq!(
            diagnostics("fn main() -> u8 { let mut x: u8 = 0; x = 1 }"),
            ["warning: unused variable `x`", "error: expected `u8`, found `()`"]
        );
        // Assignment is `()`, so chaining assignments is a type error.
        assert_eq!(
            diagnostics(&in_main("let mut x: u8 = 0; let mut y: u8 = 0; x = y = 5; x; y;")),
            ["error: expected `u8`, found `()`", "error: unused value of type `u8`", "error: unused value of type `u8`"]
        );
    }

    #[test]
    fn statement_errors() {
        assert_eq!(diagnostics(&in_main("let x: u8 = 1; x;")), ["error: unused value of type `u8`"]);
        assert_eq!(diagnostics(&in_main("let x: u8 = 1; { x }")), ["error: unused value of type `u8`"]);
    }

    #[test]
    fn name_errors() {
        assert_eq!(diagnostics("fn main() -> u8 { x }"), ["error: undefined variable `x`"]);
        // Diagnostics are in source order: the declared name comes before its initializer.
        assert_eq!(diagnostics(&in_main("let x: u8 = x;")), ["warning: unused variable `x`", "error: undefined variable `x`"]);
        assert_eq!(
            diagnostics("fn main() -> u8 { { let x: u8 = 1; } x }"),
            ["warning: unused variable `x`", "error: undefined variable `x`"]
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
        assert!(diagnostics("fn main() -> u8 { { let mut x: u8 = 1; x = x; } let x: u8 = 2; x }").is_empty());
    }

    #[test]
    fn failed_declarations_do_not_cascade() {
        // `x` has no type, but using it afterward reports nothing more.
        assert_eq!(
            diagnostics("fn main() -> u8 { let x = 5; x }"),
            ["error: cannot infer the type of integer `5`; it needs a type from its context, such as a type annotation"]
        );
        assert_eq!(diagnostics("fn main() -> u8 { let x: i32 = 5; x }"), ["error: unknown type `i32`"]);
    }

    #[test]
    fn assignment_errors() {
        assert_eq!(
            diagnostics(&in_main("let x: u8 = 1; x = 2; x;")),
            ["error: cannot assign to `x`, because it is not declared `mut`", "error: unused value of type `u8`"]
        );
        assert_eq!(diagnostics(&in_main("5 = 3;")), ["error: only variables can be assigned to"]);
        assert_eq!(diagnostics(&in_main("y = 3;")), ["error: undefined variable `y`"]);
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
    fn warnings() {
        assert_eq!(diagnostics(&in_main("let x: u8 = 1;")), ["warning: unused variable `x`"]);
        assert_eq!(
            diagnostics("fn main() -> u8 { let mut x: u8 = 1; x }"),
            ["warning: variable `x` is declared `mut` but never assigned to"]
        );
        assert_eq!(
            diagnostics(&in_main("let mut x: u8 = 1;")),
            ["warning: unused variable `x`", "warning: variable `x` is declared `mut` but never assigned to"]
        );
        // Assigning to a variable doesn't count as reading it.
        assert_eq!(diagnostics(&in_main("let mut x: u8 = 1; x = 2;")), ["warning: unused variable `x`"]);
        // A program with only warnings still compiles.
        assert!(check_str(&in_main("let x: u8 = 1;")).0.is_some());
    }
}
