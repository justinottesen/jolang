//! Builds an AST from tokens by recursive descent: each grammar rule in design/language.md is one
//! function, which consumes the tokens for that rule.

use crate::ast::{
    Ast, BinaryOp, BinaryOpKind, Expr, ExprId, ExprKind, FnDecl, Ident, Stmt, TypeExpr, UnaryOp,
};
use crate::diagnostic::Diagnostic;
use crate::lexer::{Token, TokenKind};
use crate::source::Span;

/// Parses a whole file. `tokens` must end with `Eof`, as the lexer guarantees. Stops at the first
/// syntax error.
pub fn parse(tokens: &[Token], source: &str) -> Result<Ast, Diagnostic> {
    let mut parser = Parser {
        tokens,
        source,
        pos: 0,
        ast: Ast::default(),
    };
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
        Ok(FnDecl {
            name,
            return_type,
            body,
            span,
        })
    }

    // type = IDENT | "(" ")" ;
    fn parse_type(&mut self) -> ParseResult<TypeExpr> {
        if let Some(open) = self.eat(TokenKind::LParen) {
            let close = self.expect(TokenKind::RParen)?;
            return Ok(TypeExpr::Unit(open.span.to(close.span)));
        }
        let name = self.expect_ident("type")?;
        Ok(TypeExpr::Name(name))
    }

    // block = "{" stmt* expr? "}" ;
    // stmt  = let_stmt | expr ";" | block | if_expr ;
    //
    // An expression followed by `}` is the block's final expression, not a statement.
    fn parse_block(&mut self) -> ParseResult<ExprId> {
        let open = self.expect(TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        let tail = loop {
            match self.peek().kind {
                TokenKind::RBrace => break None,
                TokenKind::Let => {
                    stmts.push(self.parse_let()?);
                    continue;
                }
                _ => {}
            }

            // A block-like expression at the start of a statement ends the statement, so
            // `{ ... } = 1` is not an assignment. Rust parses it the same way.
            let (expr, block_like) = match self.peek().kind {
                TokenKind::LBrace => (self.parse_block()?, true),
                TokenKind::If => (self.parse_if()?, true),
                _ => (self.parse_expr()?, false),
            };
            match self.peek().kind {
                TokenKind::Semicolon => {
                    self.advance();
                    stmts.push(Stmt::Expr {
                        expr,
                        semicolon: true,
                    });
                }
                TokenKind::RBrace => break Some(expr),
                _ if block_like => stmts.push(Stmt::Expr {
                    expr,
                    semicolon: false,
                }),
                _ => return Err(self.error_expected("`;` or `}`")),
            }
        };
        let close = self.expect(TokenKind::RBrace)?;

        let stmts = self.ast.add_stmts(stmts);
        let span = open.span.to(close.span);
        Ok(self.ast.add_expr(Expr {
            kind: ExprKind::Block { stmts, tail },
            span,
        }))
    }

    // let_stmt = "let" "mut"? IDENT ( ":" type )? "=" expr ";" ;
    fn parse_let(&mut self) -> ParseResult<Stmt> {
        let let_token = self.expect(TokenKind::Let)?;
        let mutable = self.eat(TokenKind::Mut).is_some();
        let name = self.expect_ident("variable name")?;
        let ty = match self.eat(TokenKind::Colon) {
            Some(_) => Some(self.parse_type()?),
            None => None,
        };
        self.expect(TokenKind::Eq)?;
        let init = self.parse_expr()?;
        let semicolon = self.expect(TokenKind::Semicolon)?;
        Ok(Stmt::Let {
            mutable,
            name,
            ty,
            init,
            span: let_token.span.to(semicolon.span),
        })
    }

    // expr = binary ( "=" expr )? ;
    fn parse_expr(&mut self) -> ParseResult<ExprId> {
        let target = self.parse_binary(0)?;
        if self.eat(TokenKind::Eq).is_none() {
            return Ok(target);
        }
        let value = self.parse_expr()?;
        let span = self.span_of(target).to(self.span_of(value));
        Ok(self.ast.add_expr(Expr {
            kind: ExprKind::Assign { target, value },
            span,
        }))
    }

    // binary = cast ( binary_op cast )* ;
    //
    // Parses by precedence climbing: only operators that bind at least as tightly as
    // `min_precedence` are consumed here, and looser ones are left to the caller.
    fn parse_binary(&mut self, min_precedence: u8) -> ParseResult<ExprId> {
        let mut lhs = self.parse_cast()?;
        while let Some((op, precedence)) = binary_op(self.peek().kind) {
            if precedence < min_precedence {
                break;
            }
            self.advance();
            // The right side only takes operators that bind more tightly, which makes operators
            // of equal precedence left-associative: `a - b - c` is `(a - b) - c`.
            let rhs = self.parse_binary(precedence + 1)?;
            let span = self.span_of(lhs).to(self.span_of(rhs));
            lhs = self.ast.add_expr(Expr {
                kind: ExprKind::Binary { op, lhs, rhs },
                span,
            });

            // Comparisons don't associate at all: `a < b < c` is an error.
            if is_comparison(op)
                && binary_op(self.peek().kind).is_some_and(|(next, _)| is_comparison(next))
            {
                let message = "comparisons can't be chained; to compare with both sides, use `and`, as in \
                               `a < b and b < c`";
                return Err(Diagnostic::error(message, self.peek().span));
            }
        }
        Ok(lhs)
    }

    // cast = unary ( ( "as" | "as%" ) type )* ;
    fn parse_cast(&mut self) -> ParseResult<ExprId> {
        let mut operand = self.parse_unary()?;
        loop {
            let wrapping = match self.peek().kind {
                TokenKind::As => false,
                TokenKind::AsPercent => true,
                _ => return Ok(operand),
            };
            self.advance();
            let ty = self.parse_type()?;
            let span = self.span_of(operand).to(ty.span());
            operand = self.ast.add_expr(Expr {
                kind: ExprKind::Cast {
                    operand,
                    ty,
                    wrapping,
                },
                span,
            });
        }
    }

    // unary = ( "-" | "-%" | "!" ) unary | primary ;
    fn parse_unary(&mut self) -> ParseResult<ExprId> {
        let op = match self.peek().kind {
            TokenKind::Minus => UnaryOp::Neg,
            TokenKind::MinusPercent => UnaryOp::WrappingNeg,
            TokenKind::Bang => UnaryOp::Not,
            _ => return self.parse_primary(),
        };
        let op_token = self.advance();
        let operand = self.parse_unary()?;
        let span = op_token.span.to(self.span_of(operand));
        Ok(self.ast.add_expr(Expr {
            kind: ExprKind::Unary { op, operand },
            span,
        }))
    }

    // primary = INTEGER | "true" | "false" | IDENT | block | if_expr | "(" expr ")" | "(" ")" ;
    fn parse_primary(&mut self) -> ParseResult<ExprId> {
        let token = self.peek();
        let kind = match token.kind {
            TokenKind::Integer => ExprKind::Integer,
            TokenKind::True => ExprKind::Bool(true),
            TokenKind::False => ExprKind::Bool(false),
            TokenKind::Ident => ExprKind::Name(Ident { span: token.span }),
            TokenKind::LBrace => return self.parse_block(),
            TokenKind::If => return self.parse_if(),
            TokenKind::LParen => return self.parse_parenthesized(),
            _ => return Err(self.error_expected("expression")),
        };
        self.advance();
        Ok(self.ast.add_expr(Expr {
            kind,
            span: token.span,
        }))
    }

    // if_expr = "if" expr block ( "else" ( if_expr | block ) )? ;
    fn parse_if(&mut self) -> ParseResult<ExprId> {
        let if_token = self.expect(TokenKind::If)?;
        let cond = self.parse_expr()?;
        let then_branch = self.parse_block()?;
        let else_branch = match self.eat(TokenKind::Else) {
            None => None,
            Some(_) => Some(match self.peek().kind {
                TokenKind::If => self.parse_if()?,
                TokenKind::LBrace => self.parse_block()?,
                _ => return Err(self.error_expected("`{` or `if`")),
            }),
        };
        let span = if_token
            .span
            .to(self.span_of(else_branch.unwrap_or(then_branch)));
        Ok(self.ast.add_expr(Expr {
            kind: ExprKind::If {
                cond,
                then_branch,
                else_branch,
            },
            span,
        }))
    }

    /// Parses `()`, or an expression in parentheses. Parentheses only group, so they don't appear
    /// in the AST.
    fn parse_parenthesized(&mut self) -> ParseResult<ExprId> {
        let open = self.expect(TokenKind::LParen)?;
        if let Some(close) = self.eat(TokenKind::RParen) {
            return Ok(self.ast.add_expr(Expr {
                kind: ExprKind::Unit,
                span: open.span.to(close.span),
            }));
        }
        let inner = self.parse_expr()?;
        self.expect(TokenKind::RParen)?;
        Ok(inner)
    }

    fn span_of(&self, expr: ExprId) -> Span {
        self.ast.expr(expr).span
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

    /// Consumes the current token if it's of the given kind.
    fn eat(&mut self, kind: TokenKind) -> Option<Token> {
        if self.peek().kind == kind {
            Some(self.advance())
        } else {
            None
        }
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
            Ok(Ident {
                span: self.advance().span,
            })
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

        // If the unexpected token is on a later line, or is the end of the file, the missing code
        // belongs at the end of the previous line, so point just after the previous token. This
        // is where a missing `;` goes, for example.
        let span = match self.pos.checked_sub(1).map(|i| self.tokens[i].span) {
            Some(previous)
                if found.kind == TokenKind::Eof
                    || self.source[previous.end as usize..found.span.start as usize]
                        .contains('\n') =>
            {
                Span {
                    start: previous.end,
                    end: previous.end,
                    ..previous
                }
            }
            _ => found.span,
        };

        Diagnostic::error(format!("expected {expected}, found {description}"), span)
    }
}

/// The binary operator a token stands for, and its precedence. Higher numbers bind more tightly.
fn binary_op(kind: TokenKind) -> Option<(BinaryOp, u8)> {
    let op = match kind {
        TokenKind::Star => (BinaryOp::Mul, 5),
        TokenKind::Slash => (BinaryOp::Div, 5),
        TokenKind::Percent => (BinaryOp::Rem, 5),
        TokenKind::StarPercent => (BinaryOp::WrappingMul, 5),
        TokenKind::Plus => (BinaryOp::Add, 4),
        TokenKind::Minus => (BinaryOp::Sub, 4),
        TokenKind::PlusPercent => (BinaryOp::WrappingAdd, 4),
        TokenKind::MinusPercent => (BinaryOp::WrappingSub, 4),
        TokenKind::EqEq => (BinaryOp::Eq, 3),
        TokenKind::NotEq => (BinaryOp::Ne, 3),
        TokenKind::Lt => (BinaryOp::Lt, 3),
        TokenKind::Le => (BinaryOp::Le, 3),
        TokenKind::Gt => (BinaryOp::Gt, 3),
        TokenKind::Ge => (BinaryOp::Ge, 3),
        TokenKind::And => (BinaryOp::And, 2),
        TokenKind::Or => (BinaryOp::Or, 1),
        _ => return None,
    };
    Some(op)
}

fn is_comparison(op: BinaryOp) -> bool {
    matches!(op.kind(), BinaryOpKind::Equality | BinaryOpKind::Ordering)
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
        assert!(
            diagnostics.is_empty(),
            "unexpected lexer diagnostics: {diagnostics:?}"
        );
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
        let TypeExpr::Name(return_type) = decl.return_type else {
            panic!("return type is not a name")
        };
        assert_eq!(sources.text(return_type.span), "u8");

        let body = ast.expr(decl.body);
        assert_eq!(sources.text(body.span), "{ 0 }");
        let ExprKind::Block {
            stmts,
            tail: Some(tail),
        } = body.kind
        else {
            panic!("unexpected body: {body:?}")
        };
        assert!(ast.stmts(stmts).is_empty());
        let tail = ast.expr(tail);
        assert!(matches!(tail.kind, ExprKind::Integer));
        assert_eq!(sources.text(tail.span), "0");
    }

    /// Parses a function body and returns the AST dump of it, without spans.
    fn dump_body(body: &str) -> String {
        let text = format!("fn main() -> u8 {body}");
        let (ast, sources) = parse_str(&text);
        let dump = ast.unwrap().dump(&sources);
        // Drop the function and return type lines, and the spans.
        let lines: Vec<String> = dump
            .lines()
            .skip(2)
            .map(|line| line.rsplit_once(" (").unwrap().0.to_string())
            .collect();
        lines.join("\n")
    }

    #[test]
    fn statements() {
        let expected = "  \
  block
    let `x`
      type `u8`
      integer `1`
    let mut `y`
      name `x`
    statement
      assign
        name `y`
        integer `2`
    name `y`";
        assert_eq!(
            dump_body("{ let x: u8 = 1; let mut y = x; y = 2; y }"),
            expected
        );
    }

    #[test]
    fn block_without_final_expression() {
        let expected = "  \
  block
    statement
      name `x`";
        assert_eq!(dump_body("{ x; }"), expected);
        assert_eq!(dump_body("{ }"), "  block");
    }

    #[test]
    fn nested_blocks() {
        // A block followed by `}` is the final expression; anywhere else it is a statement, with or
        // without a `;`.
        let expected = "  \
  block
    statement (no `;`)
      block
        integer `1`
    statement
      block
        integer `2`
    block
      integer `3`";
        assert_eq!(dump_body("{ { 1 } { 2 }; { 3 } }"), expected);
    }

    #[test]
    fn assignment_is_right_associative() {
        let expected = "  \
  block
    assign
      name `a`
      assign
        name `b`
        integer `1`";
        assert_eq!(dump_body("{ a = b = 1 }"), expected);
    }

    /// Parses an expression, and prints it fully parenthesized, to show how it was grouped.
    fn grouping(expr: &str) -> String {
        let text = format!("fn main() -> u8 {{ {expr} }}");
        let (ast, _) = parse_str(&text);
        let ast = ast.unwrap();
        let ExprKind::Block {
            tail: Some(tail), ..
        } = ast.expr(ast.fns[0].body).kind
        else {
            panic!()
        };
        fn show(ast: &Ast, id: ExprId, text: &str) -> String {
            let expr = ast.expr(id);
            match expr.kind {
                ExprKind::Binary { op, lhs, rhs } => {
                    format!(
                        "({} {} {})",
                        show(ast, lhs, text),
                        op.symbol(),
                        show(ast, rhs, text)
                    )
                }
                ExprKind::Unary { op, operand } => {
                    format!("({}{})", op.symbol(), show(ast, operand, text))
                }
                ExprKind::Cast {
                    operand,
                    ty,
                    wrapping,
                } => {
                    let keyword = if wrapping { "as%" } else { "as" };
                    let ty = &text[ty.span().start as usize..ty.span().end as usize];
                    format!("({} {keyword} {ty})", show(ast, operand, text))
                }
                ExprKind::Assign { target, value } => {
                    format!("({} = {})", show(ast, target, text), show(ast, value, text))
                }
                _ => text[expr.span.start as usize..expr.span.end as usize].to_string(),
            }
        }
        show(&ast, tail, &text)
    }

    #[test]
    fn precedence() {
        assert_eq!(grouping("a + b * c"), "(a + (b * c))");
        assert_eq!(grouping("a * b + c"), "((a * b) + c)");
        assert_eq!(grouping("a - b - c"), "((a - b) - c)");
        assert_eq!(grouping("a / b % c *% d"), "(((a / b) % c) *% d)");
        assert_eq!(grouping("a +% b -% c + d"), "(((a +% b) -% c) + d)");
        assert_eq!(grouping("(a + b) * c"), "((a + b) * c)");
        assert_eq!(grouping("-a * b"), "((-a) * b)");
        assert_eq!(grouping("- -% a"), "(-(-%a))");
        assert_eq!(grouping("a - -b"), "(a - (-b))");
        assert_eq!(grouping("a * b as u8"), "(a * (b as u8))");
        assert_eq!(grouping("-a as u8"), "((-a) as u8)");
        assert_eq!(grouping("a as u16 as% u8"), "((a as u16) as% u8)");
        assert_eq!(grouping("x = a + b"), "(x = (a + b))");
        assert_eq!(grouping("x = y = a"), "(x = (y = a))");
        assert_eq!(grouping("a + 1 < b * 2"), "((a + 1) < (b * 2))");
        assert_eq!(
            grouping("a < b and c == d or e"),
            "(((a < b) and (c == d)) or e)"
        );
        assert_eq!(grouping("a or b and c"), "(a or (b and c))");
        assert_eq!(grouping("a and b and c"), "((a and b) and c)");
        assert_eq!(grouping("!a and !b"), "((!a) and (!b))");
        assert_eq!(grouping("x = a == b"), "(x = (a == b))");
    }

    #[test]
    fn if_expressions() {
        let expected = "  \
  block
    let `x`
      if
        name `c`
        block
          integer `1`
        if
          name `d`
          block
            integer `2`
          block
            integer `3`
    statement (no `;`)
      if
        name `c`
        block
          assign
            name `y`
            integer `4`
    name `x`";
        assert_eq!(
            dump_body("{ let x = if c { 1 } else if d { 2 } else { 3 }; if c { y = 4 } x }"),
            expected
        );
    }

    #[test]
    fn bool_literals() {
        assert_eq!(grouping("true and false"), "(true and false)");
    }

    #[test]
    fn parentheses_and_unit() {
        let (ast, sources) = parse_str("fn main() -> u8 { let u: () = (); ((x)) }");
        let ast = ast.unwrap();
        let ExprKind::Block {
            stmts,
            tail: Some(tail),
        } = ast.expr(ast.fns[0].body).kind
        else {
            panic!()
        };
        let Stmt::Let { init, .. } = ast.stmts(stmts)[0] else {
            panic!()
        };
        assert!(matches!(ast.expr(init).kind, ExprKind::Unit));
        // Parentheses only group; the expression inside is used directly.
        assert!(matches!(ast.expr(tail).kind, ExprKind::Name(_)));
        assert_eq!(sources.text(ast.expr(tail).span), "x");
    }

    #[test]
    fn block_statement_ends_before_operator() {
        // As in Rust, a block at the start of a statement is a whole statement, so `- 1` is a
        // separate expression rather than a subtraction.
        let expected = "  \
  block
    statement (no `;`)
      block
        name `x`
    unary `-`
      integer `1`";
        assert_eq!(dump_body("{ { x } - 1 }"), expected);
    }

    #[test]
    fn unit_type() {
        let (ast, sources) = parse_str("fn f() -> ( ) { }");
        let decl = ast.unwrap().fns[0];
        assert!(matches!(decl.return_type, TypeExpr::Unit(_)));
        assert_eq!(sources.text(decl.return_type.span()), "( )");
    }

    #[test]
    fn empty_file() {
        assert!(parse_str("").0.unwrap().fns.is_empty());
    }

    #[test]
    fn multiple_functions() {
        // The grammar allows any number of functions; the checker restricts it.
        let (ast, sources) = parse_str("fn a() -> u8 { 1 } fn b() -> u8 { 2 }");
        let names: Vec<&str> = ast
            .unwrap()
            .fns
            .iter()
            .map(|f| sources.text(f.name.span))
            .collect();
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
            ("fn main() -> u8 { ; }", "expected expression, found `;`"),
            (
                "fn main() -> u8 { 0 1 }",
                "expected `;` or `}`, found integer `1`",
            ),
            (
                "fn main() -> u8 { 0",
                "expected `;` or `}`, found end of file",
            ),
            (
                "fn main() -> u8 {",
                "expected expression, found end of file",
            ),
            ("fn main() -> ( { }", "expected `)`, found `{`"),
            (
                "fn main() -> u8 { let = 1; }",
                "expected variable name, found `=`",
            ),
            (
                "fn main() -> u8 { let mut mut x = 1; }",
                "expected variable name, found `mut`",
            ),
            (
                "fn main() -> u8 { let x 1; }",
                "expected `=`, found integer `1`",
            ),
            (
                "fn main() -> u8 { let x: = 1; }",
                "expected type, found `=`",
            ),
            ("fn main() -> u8 { let x = 1 }", "expected `;`, found `}`"),
            (
                "fn main() -> u8 { let x = ; }",
                "expected expression, found `;`",
            ),
            ("fn main() -> u8 { x = }", "expected expression, found `}`"),
            (
                "fn main() -> u8 { { 1 } = 2; }",
                "expected expression, found `=`",
            ),
            ("fn main() -> u8 { 1 + }", "expected expression, found `}`"),
            (
                "fn main() -> u8 { 1 + * 2 }",
                "expected expression, found `*`",
            ),
            ("fn main() -> u8 { (1 + 2 }", "expected `)`, found `}`"),
            ("fn main() -> u8 { x as }", "expected type, found `}`"),
            (
                "fn main() -> u8 { x as 5 }",
                "expected type, found integer `5`",
            ),
            (
                "fn main() -> u8 { a + %b }",
                "expected expression, found `%`",
            ),
            (
                "fn main() -> u8 { a < b < c }",
                "comparisons can't be chained; to compare with both sides, use `and`, as in `a < b and b < c`",
            ),
            (
                "fn main() -> u8 { a == b != c }",
                "comparisons can't be chained; to compare with both sides, use `and`, as in `a < b and b < c`",
            ),
            (
                "fn main() -> u8 { if c 1 }",
                "expected `{`, found integer `1`",
            ),
            ("fn main() -> u8 { if { 1 } }", "expected `{`, found `}`"),
            (
                "fn main() -> u8 { if c { 1 } else 2 }",
                "expected `{` or `if`, found integer `2`",
            ),
            (
                "fn main() -> u8 { else { 1 } }",
                "expected expression, found `else`",
            ),
        ];
        for (text, message) in cases {
            assert_eq!(parse_error(text).message, message, "for {text:?}");
        }
    }

    #[test]
    fn error_on_next_line_points_after_previous_token() {
        let text = "fn main() -> u8 {\n    let x: u8 = 1\n    x\n}";
        let diagnostic = parse_error(text);
        assert_eq!(diagnostic.message, "expected `;`, found identifier `x`");
        let end_of_one = text.find("1\n").unwrap() as u32 + 1;
        assert_eq!(
            (diagnostic.span.start, diagnostic.span.end),
            (end_of_one, end_of_one)
        );

        // On the same line, the error points at the unexpected token itself.
        let diagnostic = parse_error("fn main() -> u8 { let x: u8 = 1 x }");
        assert_eq!((diagnostic.span.start, diagnostic.span.end), (32, 33));
    }

    #[test]
    fn error_at_end_of_file_points_after_last_token() {
        let diagnostic = parse_error("fn main() -> u8 { 0\n\n\n");
        // `{ 0` is a valid start, so the error is at the end of the `0`.
        assert_eq!((diagnostic.span.start, diagnostic.span.end), (19, 19));
    }
}
