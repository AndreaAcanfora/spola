//! Turns source text into tokens.
//!
//! Newlines end statements the way they do in Go: a newline after a token that can end an expression (a name, a
//! literal, `)`, `]`, `}`, `return`, `break`, `continue`) becomes a `;`. So semicolons are optional, and a line that
//! continues an expression should end with an operator or an open bracket.

use crate::error::{Error, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Int(i64),
    Float(f64),
    /// A string literal, split into literal text and `{expression}` interpolations.
    Str(Vec<StrPart>),
    Ident(String),
    // keywords
    Let,
    Fn,
    Return,
    If,
    Else,
    While,
    For,
    In,
    Break,
    Continue,
    True,
    False,
    Nil,
    And,
    Or,
    Not,
    Await,
    // punctuation
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Dot,
    Colon,
    Semi,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Assign,
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    Lit(String),
    /// The source of an interpolated expression and where it starts.
    Expr(String, Span),
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

pub fn lex(src: &str) -> Result<Vec<Token>, Error> {
    lex_at(src, Span { line: 1, col: 1 })
}

/// Lexes `src` as if it started at `start`, so that interpolated expressions report their real position.
pub fn lex_at(src: &str, start: Span) -> Result<Vec<Token>, Error> {
    let mut lx = Lexer {
        chars: src.chars().collect(),
        pos: 0,
        line: start.line,
        col: start.col,
        out: Vec::new(),
    };
    lx.run()?;
    Ok(lx.out)
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: u32,
    col: u32,
    out: Vec<Token>,
}

impl Lexer {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<char> {
        self.chars.get(self.pos + 1).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn span(&self) -> Span {
        Span {
            line: self.line,
            col: self.col,
        }
    }

    fn push(&mut self, tok: Tok, span: Span) {
        self.out.push(Token { tok, span });
    }

    /// Whether a newline after the last token ends a statement.
    fn ends_statement(&self) -> bool {
        matches!(
            self.out.last().map(|t| &t.tok),
            Some(
                Tok::Int(_)
                    | Tok::Float(_)
                    | Tok::Str(_)
                    | Tok::Ident(_)
                    | Tok::True
                    | Tok::False
                    | Tok::Nil
                    | Tok::RParen
                    | Tok::RBracket
                    | Tok::RBrace
                    | Tok::Return
                    | Tok::Break
                    | Tok::Continue
            )
        )
    }

    fn run(&mut self) -> Result<(), Error> {
        while let Some(c) = self.peek() {
            let span = self.span();
            match c {
                '\n' => {
                    if self.ends_statement() {
                        self.push(Tok::Semi, span);
                    }
                    self.bump();
                }
                ' ' | '\t' | '\r' => {
                    self.bump();
                }
                '/' if self.peek2() == Some('/') => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.bump();
                    }
                }
                '"' => {
                    let tok = self.string()?;
                    self.push(tok, span);
                }
                c if c.is_ascii_digit() => {
                    let tok = self.number(span)?;
                    self.push(tok, span);
                }
                c if c.is_alphabetic() || c == '_' => {
                    let mut word = String::new();
                    while let Some(c) = self.peek().filter(|c| c.is_alphanumeric() || *c == '_') {
                        word.push(c);
                        self.bump();
                    }
                    self.push(keyword(&word).unwrap_or(Tok::Ident(word)), span);
                }
                _ => {
                    self.bump();
                    let next = self.peek();
                    let two = |lx: &mut Lexer, t: Tok| {
                        lx.bump();
                        t
                    };
                    let tok = match (c, next) {
                        ('=', Some('=')) => two(self, Tok::Eq),
                        ('!', Some('=')) => two(self, Tok::Ne),
                        ('<', Some('=')) => two(self, Tok::Le),
                        ('>', Some('=')) => two(self, Tok::Ge),
                        ('+', Some('=')) => two(self, Tok::PlusAssign),
                        ('-', Some('=')) => two(self, Tok::MinusAssign),
                        ('*', Some('=')) => two(self, Tok::StarAssign),
                        ('/', Some('=')) => two(self, Tok::SlashAssign),
                        ('(', _) => Tok::LParen,
                        (')', _) => Tok::RParen,
                        ('[', _) => Tok::LBracket,
                        (']', _) => Tok::RBracket,
                        ('{', _) => Tok::LBrace,
                        ('}', _) => Tok::RBrace,
                        (',', _) => Tok::Comma,
                        ('.', _) => Tok::Dot,
                        (':', _) => Tok::Colon,
                        (';', _) => Tok::Semi,
                        ('+', _) => Tok::Plus,
                        ('-', _) => Tok::Minus,
                        ('*', _) => Tok::Star,
                        ('/', _) => Tok::Slash,
                        ('%', _) => Tok::Percent,
                        ('=', _) => Tok::Assign,
                        ('<', _) => Tok::Lt,
                        ('>', _) => Tok::Gt,
                        ('!', _) => return Err(Error::syntax("use `not` for negation", span)),
                        _ => return Err(Error::syntax(format!("unexpected character `{c}`"), span)),
                    };
                    self.push(tok, span);
                }
            }
        }
        if self.ends_statement() {
            let span = self.span();
            self.push(Tok::Semi, span);
        }
        let span = self.span();
        self.push(Tok::Eof, span);
        Ok(())
    }

    fn number(&mut self, span: Span) -> Result<Tok, Error> {
        let mut text = String::new();
        let mut is_float = false;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '_' {
                if c != '_' {
                    text.push(c);
                }
                self.bump();
            } else if c == '.' && !is_float && self.peek2().is_some_and(|d| d.is_ascii_digit()) {
                is_float = true;
                text.push(c);
                self.bump();
            } else {
                break;
            }
        }
        if is_float {
            text.parse()
                .map(Tok::Float)
                .map_err(|_| Error::syntax("invalid number", span))
        } else {
            text.parse()
                .map(Tok::Int)
                .map_err(|_| Error::syntax("integer literal too large", span))
        }
    }

    fn string(&mut self) -> Result<Tok, Error> {
        let start = self.span();
        self.bump(); // opening quote
        let mut parts = Vec::new();
        let mut lit = String::new();
        loop {
            let span = self.span();
            match self.bump() {
                None => return Err(Error::syntax("unterminated string", start)),
                Some('"') => break,
                Some('\\') => {
                    let c = match self.bump() {
                        Some('n') => '\n',
                        Some('t') => '\t',
                        Some('r') => '\r',
                        Some('0') => '\0',
                        Some(c @ ('"' | '\\' | '{' | '}')) => c,
                        _ => return Err(Error::syntax("unknown escape sequence", span)),
                    };
                    lit.push(c);
                }
                Some('{') => {
                    if !lit.is_empty() {
                        parts.push(StrPart::Lit(std::mem::take(&mut lit)));
                    }
                    let expr_span = self.span();
                    let expr = self.interpolation(span)?;
                    if expr.trim().is_empty() {
                        return Err(Error::syntax(
                            "empty `{}` in string; write `\\{` for a literal brace",
                            span,
                        ));
                    }
                    parts.push(StrPart::Expr(expr, expr_span));
                }
                Some('}') => {
                    return Err(Error::syntax(
                        "unmatched `}` in string; write `\\}` for a literal brace",
                        span,
                    ));
                }
                Some(c) => lit.push(c),
            }
        }
        if !lit.is_empty() || parts.is_empty() {
            parts.push(StrPart::Lit(lit));
        }
        Ok(Tok::Str(parts))
    }

    /// Reads the source of a `{...}` interpolation up to its closing brace, skipping nested braces and strings.
    fn interpolation(&mut self, open: Span) -> Result<String, Error> {
        let mut depth = 0;
        let mut text = String::new();
        loop {
            match self.bump() {
                None | Some('\n') => return Err(Error::syntax("unterminated `{` in string", open)),
                Some('}') if depth == 0 => return Ok(text),
                Some(c) => {
                    match c {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        '"' => {
                            // a string inside the interpolation: copy it whole
                            text.push(c);
                            loop {
                                match self.bump() {
                                    None | Some('\n') => return Err(Error::syntax("unterminated string", open)),
                                    Some('\\') => {
                                        text.push('\\');
                                        if let Some(e) = self.bump() {
                                            text.push(e);
                                        }
                                    }
                                    Some('"') => break,
                                    Some(c) => text.push(c),
                                }
                            }
                        }
                        _ => {}
                    }
                    text.push(c);
                }
            }
        }
    }
}

fn keyword(word: &str) -> Option<Tok> {
    Some(match word {
        "let" => Tok::Let,
        "fn" => Tok::Fn,
        "return" => Tok::Return,
        "if" => Tok::If,
        "else" => Tok::Else,
        "while" => Tok::While,
        "for" => Tok::For,
        "in" => Tok::In,
        "break" => Tok::Break,
        "continue" => Tok::Continue,
        "true" => Tok::True,
        "false" => Tok::False,
        "nil" => Tok::Nil,
        "and" => Tok::And,
        "or" => Tok::Or,
        "not" => Tok::Not,
        "await" => Tok::Await,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Tok> {
        lex(src).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn newlines_end_statements() {
        assert_eq!(
            toks("let x = 1\nx +\n2"),
            vec![
                Tok::Let,
                Tok::Ident("x".into()),
                Tok::Assign,
                Tok::Int(1),
                Tok::Semi,
                Tok::Ident("x".into()),
                Tok::Plus,
                Tok::Int(2),
                Tok::Semi,
                Tok::Eof
            ]
        );
    }

    #[test]
    fn strings_interpolate() {
        let t = toks(r#""a{x + "}"}b""#);
        assert_eq!(
            t[0],
            Tok::Str(vec![
                StrPart::Lit("a".into()),
                StrPart::Expr(r#"x + "}""#.into(), Span { line: 1, col: 4 }),
                StrPart::Lit("b".into())
            ])
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(
            toks("1_000 2.5 3.x")[..4],
            [Tok::Int(1000), Tok::Float(2.5), Tok::Int(3), Tok::Dot]
        );
    }
}
