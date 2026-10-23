//! Checks that a program follows the language's rules, and builds its HIR.
//!
//! Type checking is bidirectional: the type an expression must have flows down into it from its
//! context. The only context so far is a function's declared return type, which the body is
//! checked against. See "Checker" in design/compiler.md.

use crate::ast::{self, Ast, TypeExpr};
use crate::diagnostic::Diagnostic;
use crate::hir::{self, Hir, Type};
use crate::intern::{Interner, Symbol};
use crate::source::{FileId, SourceMap, Span};

/// Checks a parsed file. Reports every error found, not just the first.
pub fn check(ast: &Ast, file: FileId, sources: &SourceMap, symbols: &mut Interner) -> Result<Hir, Vec<Diagnostic>> {
    let mut checker = Checker { ast, sources, symbols, hir: Hir::default(), diagnostics: Vec::new() };
    checker.check_file(file);
    if checker.diagnostics.is_empty() { Ok(checker.hir) } else { Err(checker.diagnostics) }
}

/// Proof that an error was reported. Functions that fail return this instead of a value, so their
/// callers can skip whatever depended on it without reporting another, redundant error.
struct Reported;

type CheckResult<T> = Result<T, Reported>;

struct Checker<'a> {
    ast: &'a Ast,
    sources: &'a SourceMap,
    symbols: &'a mut Interner,
    hir: Hir,
    diagnostics: Vec<Diagnostic>,
}

impl Checker<'_> {
    fn error(&mut self, message: impl Into<String>, span: Span) -> Reported {
        self.diagnostics.push(Diagnostic::error(message, span));
        Reported
    }

    fn check_file(&mut self, file: FileId) {
        let main = self.symbols.intern("main");
        let mut found_main = false;

        for decl in &self.ast.fns {
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
        let return_type = self.resolve_type(decl.return_type)?;
        if name == main && return_type != Type::U8 {
            // Unreachable while `u8` is the only type.
            let span = match decl.return_type {
                TypeExpr::Name(name) => name.span,
            };
            self.error(format!("`main` must return `u8`, not `{return_type}`"), span);
        }
        let body = self.check_expr(decl.body, return_type)?;
        Ok(hir::Function { name, return_type, body, span: decl.span })
    }

    fn resolve_type(&mut self, ty: TypeExpr) -> CheckResult<Type> {
        match ty {
            TypeExpr::Name(name) => match self.sources.text(name.span) {
                "u8" => Ok(Type::U8),
                text => Err(self.error(format!("unknown type `{text}`"), name.span)),
            },
        }
    }

    /// Checks an expression that must have type `expected`.
    fn check_expr(&mut self, id: ast::ExprId, expected: Type) -> CheckResult<hir::ExprId> {
        let expr = *self.ast.expr(id);
        let kind = match expr.kind {
            ast::ExprKind::Integer => {
                let value = self.integer_value(expr.span, expected)?;
                hir::ExprKind::Integer { value }
            }
            ast::ExprKind::Block { tail } => {
                let tail = self.check_expr(tail, expected)?;
                hir::ExprKind::Block { tail }
            }
        };
        Ok(self.hir.add_expr(hir::Expr { kind, ty: expected, span: expr.span }))
    }

    /// Parses an integer literal's value, and checks that it fits in `ty`.
    fn integer_value(&mut self, span: Span, ty: Type) -> CheckResult<u128> {
        let text = self.sources.text(span);
        // The lexer guarantees the literal is only decimal digits. Overflowing a `u128` means it's
        // too large for any type.
        let value = text
            .bytes()
            .try_fold(0u128, |value, digit| value.checked_mul(10)?.checked_add((digit - b'0').into()));
        match value {
            Some(value) if value <= ty.max_value() => Ok(value),
            _ => {
                let message = format!("`{text}` does not fit in `{ty}` (range 0 to {})", ty.max_value());
                Err(self.error(message, span))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer, parser};

    fn check_str_with_symbols(text: &str) -> (Result<Hir, Vec<Diagnostic>>, Interner) {
        let mut sources = SourceMap::default();
        let file = sources.add("test.jo".into(), text.into()).unwrap();
        let (tokens, diagnostics) = lexer::lex(file, text);
        assert!(diagnostics.is_empty(), "unexpected lexer diagnostics: {diagnostics:?}");
        let ast = parser::parse(&tokens, text).expect("unexpected parse error");
        let mut symbols = Interner::default();
        (check(&ast, file, &sources, &mut symbols), symbols)
    }

    fn check_str(text: &str) -> Result<Hir, Vec<Diagnostic>> {
        check_str_with_symbols(text).0
    }

    fn error_messages(text: &str) -> Vec<String> {
        match check_str(text) {
            Ok(_) => panic!("expected errors for {text:?}"),
            Err(diagnostics) => diagnostics.into_iter().map(|d| d.message).collect(),
        }
    }

    #[test]
    fn minimal_program() {
        let (hir, symbols) = check_str_with_symbols("fn main() -> u8 { 42 }");
        let hir = hir.unwrap();
        assert_eq!(hir.fns.len(), 1);
        let main = hir.fns[0];
        assert_eq!(symbols.get(main.name), "main");
        assert_eq!(main.return_type, Type::U8);

        let body = hir.expr(main.body);
        assert_eq!(body.ty, Type::U8);
        let hir::ExprKind::Block { tail } = body.kind else { panic!("body is not a block: {body:?}") };
        let tail = hir.expr(tail);
        assert_eq!(tail.ty, Type::U8);
        assert!(matches!(tail.kind, hir::ExprKind::Integer { value: 42 }));
    }

    #[test]
    fn integer_range() {
        assert!(check_str("fn main() -> u8 { 0 }").is_ok());
        assert!(check_str("fn main() -> u8 { 255 }").is_ok());
        assert!(check_str("fn main() -> u8 { 000255 }").is_ok());
        assert_eq!(error_messages("fn main() -> u8 { 256 }"), ["`256` does not fit in `u8` (range 0 to 255)"]);
        // Larger than a u128.
        let huge = "9".repeat(50);
        assert_eq!(
            error_messages(&format!("fn main() -> u8 {{ {huge} }}")),
            [format!("`{huge}` does not fit in `u8` (range 0 to 255)")]
        );
    }

    #[test]
    fn error_messages_for_rules() {
        assert_eq!(error_messages(""), ["no `main` function"]);
        assert_eq!(error_messages("fn main() -> i32 { 0 }"), ["unknown type `i32`"]);
        assert_eq!(
            error_messages("fn main() -> u8 { 0 } fn main() -> u8 { 0 }"),
            ["`main` is defined more than once"]
        );
        assert_eq!(
            error_messages("fn other() -> u8 { 0 }"),
            ["functions other than `main` are not supported yet", "no `main` function"]
        );
    }

    #[test]
    fn reports_every_error() {
        assert_eq!(
            error_messages("fn main() -> i32 { 0 } fn other() -> u8 { 300 }"),
            [
                "unknown type `i32`",
                "functions other than `main` are not supported yet",
                "`300` does not fit in `u8` (range 0 to 255)",
            ]
        );
    }
}
