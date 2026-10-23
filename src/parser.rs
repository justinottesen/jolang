//! Builds an AST from tokens by recursive descent: each grammar rule in design/language.md is one
//! function, which consumes the tokens for that rule.

use crate::ast::{Ast, Expr, ExprId, ExprKind, FnDecl, Ident, TypeExpr};
use crate::diagnostic::Diagnostic;
use crate::lexer::{Token, TokenKind};
use crate::source::Span;

/// Parses a whole file. `tokens` must end with `Eof`, as the lexer guarantees. Stops at the first
/// syntax error.
pub fn parse(tokens: &[Token], source: &str) -> Result<Ast, Diagnostic> {
    let mut parser = Parser { tokens, source, pos: 0, ast: Ast::default() };
    parser.parse_file()?;
    Ok(parser.ast)
}

struct Parser<'a> {
    tokens: &'a [Token],
    source: &'a str,
    pos: usize,
    ast: Ast,
}

type ParseResult<T> = Result<T, Diagnostic>;

impl Parser<'_> {
    // file = fn_decl* ;
    fn parse_file(&mut self) -> ParseResult<()> {
        while self.peek().kind != TokenKind::Eof {
            let decl = self.parse_fn_decl()?;
            self.ast.fns.push(decl);
        }
        Ok(())
    }

    // fn_decl = "fn" IDENT "(" ")" "->" type block ;
    fn parse_fn_decl(&mut self) -> ParseResult<FnDecl> {
        let fn_token = self.expect(TokenKind::Fn)?;
        let name = self.expect_ident("function name")?;
        self.expect(TokenKind::LParen)?;
        self.expect(TokenKind::RParen)?;
        self.expect(TokenKind::Arrow)?;
        let return_type = self.parse_type()?;
        let body = self.parse_block()?;
        let span = fn_token.span.to(self.ast.expr(body).span);
        Ok(FnDecl { name, return_type, body, span })
    }

    // type = IDENT ;
    fn parse_type(&mut self) -> ParseResult<TypeExpr> {
        let name = self.expect_ident("type")?;
        Ok(TypeExpr::Name(name))
    }

    // block = "{" expr "}" ;
    fn parse_block(&mut self) -> ParseResult<ExprId> {
        let open = self.expect(TokenKind::LBrace)?;
        let tail = self.parse_expr()?;
        let close = self.expect(TokenKind::RBrace)?;
        let span = open.span.to(close.span);
        Ok(self.ast.add_expr(Expr { kind: ExprKind::Block { tail }, span }))
    }

    // expr = INTEGER ;
    fn parse_expr(&mut self) -> ParseResult<ExprId> {
        let token = self.peek();
        match token.kind {
            TokenKind::Integer => {
                self.advance();
                Ok(self.ast.add_expr(Expr { kind: ExprKind::Integer, span: token.span }))
            }
            _ => Err(self.error_expected("expression")),
        }
    }

    fn peek(&self) -> Token {
        self.tokens[self.pos]
    }

    /// Consumes the current token. Never moves past `Eof`.
    fn advance(&mut self) -> Token {
        let token = self.peek();
        if token.kind != TokenKind::Eof {
            self.pos += 1;
        }
        token
    }

    fn expect(&mut self, kind: TokenKind) -> ParseResult<Token> {
        if self.peek().kind == kind {
            Ok(self.advance())
        } else {
            Err(self.error_expected(kind.describe()))
        }
    }

    /// Expects an identifier. `what` describes the role it plays, for the error message.
    fn expect_ident(&mut self, what: &str) -> ParseResult<Ident> {
        if self.peek().kind == TokenKind::Ident {
            Ok(Ident { span: self.advance().span })
        } else {
            Err(self.error_expected(what))
        }
    }

    /// An error for when the current token isn't what the grammar requires.
    fn error_expected(&self, expected: &str) -> Diagnostic {
        let found = self.peek();
        let description = match found.kind {
            TokenKind::Ident | TokenKind::Integer => {
                let text = &self.source[found.span.start as usize..found.span.end as usize];
                format!("{} `{text}`", found.kind.describe())
            }
            _ => found.kind.describe().to_string(),
        };

        // At the end of the file, point just after the last token instead of at the very end,
        // which may be several lines later.
        let span = match found.kind {
            TokenKind::Eof if self.pos > 0 => {
                let previous = self.tokens[self.pos - 1].span;
                Span { start: previous.end, ..previous }
            }
            _ => found.span,
        };

        Diagnostic::error(format!("expected {expected}, found {description}"), span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer;
    use crate::source::SourceMap;

    fn parse_str(text: &str) -> (Result<Ast, Diagnostic>, SourceMap) {
        let mut sources = SourceMap::default();
        let file = sources.add("test.jo".into(), text.into()).unwrap();
        let (tokens, diagnostics) = lexer::lex(file, text);
        assert!(diagnostics.is_empty(), "unexpected lexer diagnostics: {diagnostics:?}");
        (parse(&tokens, text), sources)
    }

    fn parse_error(text: &str) -> Diagnostic {
        match parse_str(text).0 {
            Ok(_) => panic!("expected a parse error for {text:?}"),
            Err(diagnostic) => diagnostic,
        }
    }

    #[test]
    fn minimal_program() {
        let text = "fn main() -> u8 { 0 }";
        let (ast, sources) = parse_str(text);
        let ast = ast.unwrap();

        assert_eq!(ast.fns.len(), 1);
        let decl = &ast.fns[0];
        assert_eq!(sources.text(decl.name.span), "main");
        assert_eq!(sources.text(decl.span), text);
        let TypeExpr::Name(return_type) = decl.return_type;
        assert_eq!(sources.text(return_type.span), "u8");

        let body = ast.expr(decl.body);
        assert_eq!(sources.text(body.span), "{ 0 }");
        let ExprKind::Block { tail } = body.kind else { panic!("body is not a block: {body:?}") };
        let tail = ast.expr(tail);
        assert!(matches!(tail.kind, ExprKind::Integer));
        assert_eq!(sources.text(tail.span), "0");
    }

    #[test]
    fn empty_file() {
        assert!(parse_str("").0.unwrap().fns.is_empty());
    }

    #[test]
    fn multiple_functions() {
        // The grammar allows any number of functions; the checker restricts it.
        let (ast, sources) = parse_str("fn a() -> u8 { 1 } fn b() -> u8 { 2 }");
        let names: Vec<&str> = ast.unwrap().fns.iter().map(|f| sources.text(f.name.span)).collect();
        assert_eq!(names, ["a", "b"]);
    }

    #[test]
    fn error_messages() {
        let cases = [
            ("0", "expected `fn`, found integer `0`"),
            ("fn () -> u8 { 0 }", "expected function name, found `(`"),
            ("fn fn() -> u8 { 0 }", "expected function name, found `fn`"),
            ("fn main) -> u8 { 0 }", "expected `(`, found `)`"),
            ("fn main() { 0 }", "expected `->`, found `{`"),
            ("fn main() -> { 0 }", "expected type, found `{`"),
            ("fn main() -> u8 0", "expected `{`, found integer `0`"),
            ("fn main() -> u8 { }", "expected expression, found `}`"),
            ("fn main() -> u8 { x }", "expected expression, found identifier `x`"),
            ("fn main() -> u8 { 0 1 }", "expected `}`, found integer `1`"),
            ("fn main() -> u8 { 0", "expected `}`, found end of file"),
        ];
        for (text, message) in cases {
            assert_eq!(parse_error(text).message, message, "for {text:?}");
        }
    }

    #[test]
    fn error_at_end_of_file_points_after_last_token() {
        let diagnostic = parse_error("fn main() -> u8 { 0\n\n\n");
        assert_eq!((diagnostic.span.start, diagnostic.span.end), (19, 19));
    }
}
