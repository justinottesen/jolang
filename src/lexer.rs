//! Splits source text into tokens.

use crate::diagnostic::Diagnostic;
use crate::source::{FileId, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    // Keywords
    Fn,
    Let,
    Mut,
    As,
    /// `as%`, a wrapping conversion.
    AsPercent,
    If,
    Else,
    True,
    False,
    And,
    Or,

    Ident,
    Integer,

    // Punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    Arrow,
    Semicolon,
    Colon,
    Eq,

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    PlusPercent,
    MinusPercent,
    StarPercent,
    Bang,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,

    /// Marks the end of the file. Always the last token.
    Eof,
}

impl TokenKind {
    /// Describes this kind of token, for error messages.
    pub fn describe(self) -> &'static str {
        match self {
            TokenKind::Fn => "`fn`",
            TokenKind::Let => "`let`",
            TokenKind::Mut => "`mut`",
            TokenKind::As => "`as`",
            TokenKind::AsPercent => "`as%`",
            TokenKind::If => "`if`",
            TokenKind::Else => "`else`",
            TokenKind::True => "`true`",
            TokenKind::False => "`false`",
            TokenKind::And => "`and`",
            TokenKind::Or => "`or`",
            TokenKind::Ident => "identifier",
            TokenKind::Integer => "integer",
            TokenKind::LParen => "`(`",
            TokenKind::RParen => "`)`",
            TokenKind::LBrace => "`{`",
            TokenKind::RBrace => "`}`",
            TokenKind::Arrow => "`->`",
            TokenKind::Semicolon => "`;`",
            TokenKind::Colon => "`:`",
            TokenKind::Eq => "`=`",
            TokenKind::Plus => "`+`",
            TokenKind::Minus => "`-`",
            TokenKind::Star => "`*`",
            TokenKind::Slash => "`/`",
            TokenKind::Percent => "`%`",
            TokenKind::PlusPercent => "`+%`",
            TokenKind::MinusPercent => "`-%`",
            TokenKind::StarPercent => "`*%`",
            TokenKind::Bang => "`!`",
            TokenKind::EqEq => "`==`",
            TokenKind::NotEq => "`!=`",
            TokenKind::Lt => "`<`",
            TokenKind::Le => "`<=`",
            TokenKind::Gt => "`>`",
            TokenKind::Ge => "`>=`",
            TokenKind::Eof => "end of file",
        }
    }
}

/// A token stores only its kind and location. Its text can be recovered from the source.
#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// Lexes an entire file. Invalid input is reported and skipped, so every error in the file is
/// found in one pass.
pub fn lex(file: FileId, text: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut lexer = Lexer {
        file,
        text,
        pos: 0,
        tokens: Vec::new(),
        diagnostics: Vec::new(),
    };
    lexer.run();
    (lexer.tokens, lexer.diagnostics)
}

struct Lexer<'a> {
    file: FileId,
    text: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
}

impl Lexer<'_> {
    fn run(&mut self) {
        while let Some(byte) = self.peek(0) {
            let start = self.pos;
            match byte {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                b'/' if self.peek(1) == Some(b'/') => self.skip_line_comment(),
                b'(' => self.punctuation(TokenKind::LParen, 1),
                b')' => self.punctuation(TokenKind::RParen, 1),
                b'{' => self.punctuation(TokenKind::LBrace, 1),
                b'}' => self.punctuation(TokenKind::RBrace, 1),
                b'-' if self.peek(1) == Some(b'>') => self.punctuation(TokenKind::Arrow, 2),
                b';' => self.punctuation(TokenKind::Semicolon, 1),
                b':' => self.punctuation(TokenKind::Colon, 1),
                b'=' => self.followed_by_eq(TokenKind::Eq, TokenKind::EqEq),
                b'!' => self.followed_by_eq(TokenKind::Bang, TokenKind::NotEq),
                b'<' => self.followed_by_eq(TokenKind::Lt, TokenKind::Le),
                b'>' => self.followed_by_eq(TokenKind::Gt, TokenKind::Ge),
                b'+' => self.operator(TokenKind::Plus, TokenKind::PlusPercent),
                b'-' => self.operator(TokenKind::Minus, TokenKind::MinusPercent),
                b'*' => self.operator(TokenKind::Star, TokenKind::StarPercent),
                b'/' => self.punctuation(TokenKind::Slash, 1),
                b'%' => self.punctuation(TokenKind::Percent, 1),
                b'0'..=b'9' => self.integer(start),
                b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.ident_or_keyword(start),
                _ => self.invalid_character(start),
            }
        }
        let end = self.pos;
        self.push(TokenKind::Eof, end);
    }

    fn peek(&self, ahead: usize) -> Option<u8> {
        self.text.as_bytes().get(self.pos + ahead).copied()
    }

    fn span(&self, start: usize) -> Span {
        Span {
            file: self.file,
            start: start as u32,
            end: self.pos as u32,
        }
    }

    /// Pushes a token covering everything from `start` to the current position.
    fn push(&mut self, kind: TokenKind, start: usize) {
        let span = self.span(start);
        self.tokens.push(Token { kind, span });
    }

    fn eat_while(&mut self, predicate: impl Fn(u8) -> bool) {
        while self.peek(0).is_some_and(&predicate) {
            self.pos += 1;
        }
    }

    fn skip_line_comment(&mut self) {
        self.eat_while(|b| b != b'\n');
    }

    fn punctuation(&mut self, kind: TokenKind, len: usize) {
        let start = self.pos;
        self.pos += len;
        self.push(kind, start);
    }

    /// Lexes a one-character operator, which becomes its wrapping form if immediately followed by
    /// `%`.
    fn operator(&mut self, plain: TokenKind, wrapping: TokenKind) {
        if self.peek(1) == Some(b'%') {
            self.punctuation(wrapping, 2);
        } else {
            self.punctuation(plain, 1);
        }
    }

    /// Lexes a one-character operator, which becomes a different one if immediately followed by
    /// `=`, like `<` and `<=`.
    fn followed_by_eq(&mut self, plain: TokenKind, with_eq: TokenKind) {
        if self.peek(1) == Some(b'=') {
            self.punctuation(with_eq, 2);
        } else {
            self.punctuation(plain, 1);
        }
    }

    fn integer(&mut self, start: usize) {
        // Consume anything that could continue the literal, so `123abc` is reported as one bad
        // literal instead of an integer followed by an identifier.
        self.eat_while(is_ident_continue);
        let text = &self.text[start..self.pos];
        if !text.bytes().all(|b| b.is_ascii_digit()) {
            self.diagnostics.push(Diagnostic::error(
                format!("invalid integer literal `{text}`: only decimal digits are allowed"),
                self.span(start),
            ));
        }
        self.push(TokenKind::Integer, start);
    }

    fn ident_or_keyword(&mut self, start: usize) {
        self.eat_while(is_ident_continue);
        let kind = match &self.text[start..self.pos] {
            "fn" => TokenKind::Fn,
            "let" => TokenKind::Let,
            "mut" => TokenKind::Mut,
            "as" if self.peek(0) == Some(b'%') => {
                self.pos += 1;
                TokenKind::AsPercent
            }
            "as" => TokenKind::As,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "and" => TokenKind::And,
            "or" => TokenKind::Or,
            _ => TokenKind::Ident,
        };
        self.push(kind, start);
    }

    fn invalid_character(&mut self, start: usize) {
        // Skip the whole character, which may be more than one byte.
        let c = self.text[start..].chars().next().unwrap();
        self.pos += c.len_utf8();
        self.diagnostics.push(Diagnostic::error(
            format!("invalid character {c:?}"),
            self.span(start),
        ));
    }
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceMap;

    fn lex_str(text: &str) -> (Vec<Token>, Vec<Diagnostic>, SourceMap) {
        let mut sources = SourceMap::default();
        let file = sources.add("test.jo".into(), text.into()).unwrap();
        let (tokens, diagnostics) = lex(file, &sources.get(file).text);
        (tokens, diagnostics, sources)
    }

    fn kinds(text: &str) -> Vec<TokenKind> {
        let (tokens, diagnostics, _) = lex_str(text);
        assert!(
            diagnostics.is_empty(),
            "unexpected diagnostics: {diagnostics:?}"
        );
        tokens.iter().map(|t| t.kind).collect()
    }

    #[test]
    fn minimal_program() {
        use TokenKind::*;
        assert_eq!(
            kinds("fn main() -> u8 { 0 }"),
            [
                Fn, Ident, LParen, RParen, Arrow, Ident, LBrace, Integer, RBrace, Eof
            ]
        );
    }

    #[test]
    fn token_text() {
        let (tokens, _, sources) = lex_str("fn main() -> u8 { 42 }");
        let texts: Vec<&str> = tokens.iter().map(|t| sources.text(t.span)).collect();
        assert_eq!(
            texts,
            ["fn", "main", "(", ")", "->", "u8", "{", "42", "}", ""]
        );
    }

    #[test]
    fn let_statement() {
        use TokenKind::*;
        assert_eq!(
            kinds("let mut x: u8 = 1; x = y;"),
            [
                Let, Mut, Ident, Colon, Ident, Eq, Integer, Semicolon, Ident, Eq, Ident, Semicolon,
                Eof
            ]
        );
        assert_eq!(kinds("letter mutable"), [Ident, Ident, Eof]);
    }

    #[test]
    fn operators() {
        use TokenKind::*;
        assert_eq!(
            kinds("+ - * / % +% -% *% as as% -> //"),
            [
                Plus,
                Minus,
                Star,
                Slash,
                Percent,
                PlusPercent,
                MinusPercent,
                StarPercent,
                As,
                AsPercent,
                Arrow,
                Eof
            ]
        );
        // `%` only joins an operator when it immediately follows it.
        assert_eq!(kinds("a + %b"), [Ident, Plus, Percent, Ident, Eof]);
        assert_eq!(kinds("a+%b"), [Ident, PlusPercent, Ident, Eof]);
        assert_eq!(kinds("x as %"), [Ident, As, Percent, Eof]);
        assert_eq!(kinds("ask as_ cast"), [Ident, Ident, Ident, Eof]);
    }

    #[test]
    fn comparisons_and_logic() {
        use TokenKind::*;
        assert_eq!(
            kinds("= == ! != < <= > >= ->"),
            [Eq, EqEq, Bang, NotEq, Lt, Le, Gt, Ge, Arrow, Eof]
        );
        assert_eq!(kinds("a===b"), [Ident, EqEq, Eq, Ident, Eof]);
        assert_eq!(
            kinds("if else true false and or"),
            [If, Else, True, False, And, Or, Eof]
        );
        assert_eq!(
            kinds("iffy elsewhere android order"),
            [Ident, Ident, Ident, Ident, Eof]
        );
    }

    #[test]
    fn empty_file() {
        assert_eq!(kinds(""), [TokenKind::Eof]);
    }

    #[test]
    fn whitespace_and_comments() {
        use TokenKind::*;
        assert_eq!(
            kinds("// comment\n\tfn\r\n// another\n// at end"),
            [Fn, Eof]
        );
    }

    #[test]
    fn keyword_prefix_is_identifier() {
        use TokenKind::*;
        assert_eq!(kinds("fnord fn_ _fn fn"), [Ident, Ident, Ident, Fn, Eof]);
    }

    #[test]
    fn eof_span_is_at_end() {
        let (tokens, _, _) = lex_str("fn  ");
        let eof = tokens.last().unwrap();
        assert_eq!((eof.span.start, eof.span.end), (4, 4));
    }

    #[test]
    fn invalid_characters_are_reported_and_skipped() {
        let (tokens, diagnostics, _) = lex_str("fn $ main \u{e9} # ?");
        let kinds: Vec<TokenKind> = tokens.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, [TokenKind::Fn, TokenKind::Ident, TokenKind::Eof]);
        let messages: Vec<&str> = diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "invalid character '$'",
                "invalid character '\u{e9}'",
                "invalid character '#'",
                "invalid character '?'",
            ]
        );
        // The multi-byte character is skipped as a whole.
        assert_eq!(diagnostics[1].span.end - diagnostics[1].span.start, 2);
    }

    #[test]
    fn integer_with_letters_is_one_bad_literal() {
        let (tokens, diagnostics, _) = lex_str("123abc");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].kind, TokenKind::Integer);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].message,
            "invalid integer literal `123abc`: only decimal digits are allowed"
        );
    }
}
