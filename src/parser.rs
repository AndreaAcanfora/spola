//! A recursive-descent parser from tokens to the syntax tree in [`crate::ast`].

use crate::ast::{BinOp, Expr, FnDecl, Stmt};
use crate::error::{Error, Span};
use crate::lexer::{StrPart, Tok, Token, lex, lex_at};

pub fn parse(src: &str) -> Result<Vec<Stmt>, Error> {
    let mut p = Parser {
        toks: lex(src)?,
        pos: 0,
    };
    let mut stmts = Vec::new();
    loop {
        p.skip_semis();
        if p.at(&Tok::Eof) {
            return Ok(stmts);
        }
        stmts.push(p.statement()?);
    }
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn span(&self) -> Span {
        self.toks[self.pos].span
    }

    fn at(&self, tok: &Tok) -> bool {
        self.peek() == tok
    }

    fn advance(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.at(tok) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, tok: &Tok, what: &str) -> Result<Span, Error> {
        if self.at(tok) {
            Ok(self.advance().span)
        } else {
            Err(self.unexpected(what))
        }
    }

    fn unexpected(&self, what: &str) -> Error {
        let found = match self.peek() {
            Tok::Eof => "end of input".to_string(),
            Tok::Semi => "end of line".to_string(),
            t => format!("`{}`", describe(t)),
        };
        Error::syntax(format!("expected {what}, found {found}"), self.span())
    }

    fn ident(&mut self, what: &str) -> Result<String, Error> {
        match self.peek().clone() {
            Tok::Ident(name) => {
                self.advance();
                Ok(name)
            }
            _ => Err(self.unexpected(what)),
        }
    }

    fn skip_semis(&mut self) {
        while self.eat(&Tok::Semi) {}
    }

    /// A simple statement ends with `;`, a newline, or the `}` that closes its block.
    fn end_of_statement(&mut self) -> Result<(), Error> {
        if self.eat(&Tok::Semi) || self.at(&Tok::RBrace) || self.at(&Tok::Eof) {
            Ok(())
        } else {
            Err(self.unexpected("end of statement"))
        }
    }

    fn block(&mut self) -> Result<Vec<Stmt>, Error> {
        self.expect(&Tok::LBrace, "`{`")?;
        let mut stmts = Vec::new();
        loop {
            self.skip_semis();
            if self.eat(&Tok::RBrace) {
                return Ok(stmts);
            }
            if self.at(&Tok::Eof) {
                return Err(self.unexpected("`}`"));
            }
            stmts.push(self.statement()?);
        }
    }

    fn statement(&mut self) -> Result<Stmt, Error> {
        let span = self.span();
        match self.peek() {
            Tok::Let => {
                self.advance();
                let name = self.ident("a variable name")?;
                self.expect(&Tok::Assign, "`=`")?;
                let mut value = self.expr()?;
                if let Expr::Lambda(f) = &mut value {
                    f.name = name.clone();
                }
                self.end_of_statement()?;
                Ok(Stmt::Let { name, value, span })
            }
            Tok::Fn if matches!(self.toks[self.pos + 1].tok, Tok::Ident(_)) => {
                self.advance();
                let name = self.ident("a function name")?;
                Ok(Stmt::Fn(self.function(name, span)?))
            }
            Tok::If => self.if_statement(),
            Tok::While => {
                self.advance();
                let cond = self.expr()?;
                let body = self.block()?;
                Ok(Stmt::While { cond, body })
            }
            Tok::For => {
                self.advance();
                let var = self.ident("a loop variable")?;
                self.expect(&Tok::In, "`in`")?;
                let iter = self.expr()?;
                let body = self.block()?;
                Ok(Stmt::For { var, iter, body, span })
            }
            Tok::LBrace => Ok(Stmt::Block(self.block()?)),
            Tok::Return => {
                self.advance();
                let value = if matches!(self.peek(), Tok::Semi | Tok::RBrace | Tok::Eof) {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.end_of_statement()?;
                Ok(Stmt::Return { value, span })
            }
            Tok::Break => {
                self.advance();
                self.end_of_statement()?;
                Ok(Stmt::Break(span))
            }
            Tok::Continue => {
                self.advance();
                self.end_of_statement()?;
                Ok(Stmt::Continue(span))
            }
            _ => {
                let expr = self.expr()?;
                let op = match self.peek() {
                    Tok::Assign => None,
                    Tok::PlusAssign => Some(BinOp::Add),
                    Tok::MinusAssign => Some(BinOp::Sub),
                    Tok::StarAssign => Some(BinOp::Mul),
                    Tok::SlashAssign => Some(BinOp::Div),
                    _ => {
                        self.end_of_statement()?;
                        return Ok(Stmt::Expr(expr));
                    }
                };
                let assign_span = self.advance().span;
                if !matches!(expr, Expr::Var(..) | Expr::Index(..) | Expr::Field(..)) {
                    return Err(Error::syntax(
                        "can only assign to a variable, an index or a field",
                        assign_span,
                    ));
                }
                let value = self.expr()?;
                self.end_of_statement()?;
                Ok(Stmt::Assign {
                    target: expr,
                    op,
                    value,
                    span: assign_span,
                })
            }
        }
    }

    fn if_statement(&mut self) -> Result<Stmt, Error> {
        self.expect(&Tok::If, "`if`")?;
        let cond = self.expr()?;
        let then = self.block()?;
        // allow `else` on the line after the closing brace
        if self.at(&Tok::Semi) && self.toks.get(self.pos + 1).is_some_and(|t| t.tok == Tok::Else) {
            self.advance();
        }
        let otherwise = if self.eat(&Tok::Else) {
            if self.at(&Tok::If) {
                Some(vec![self.if_statement()?])
            } else {
                Some(self.block()?)
            }
        } else {
            None
        };
        Ok(Stmt::If { cond, then, otherwise })
    }

    fn function(&mut self, name: String, span: Span) -> Result<FnDecl, Error> {
        self.expect(&Tok::LParen, "`(`")?;
        let mut params: Vec<String> = Vec::new();
        while !self.at(&Tok::RParen) {
            let pspan = self.span();
            let p = self.ident("a parameter name")?;
            if params.contains(&p) {
                return Err(Error::syntax(format!("duplicate parameter `{p}`"), pspan));
            }
            params.push(p);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen, "`)`")?;
        if params.len() > 255 {
            return Err(Error::syntax("a function can have at most 255 parameters", span));
        }
        let body = self.block()?;
        Ok(FnDecl {
            name,
            params,
            body,
            span,
        })
    }

    fn expr(&mut self) -> Result<Expr, Error> {
        self.or()
    }

    fn or(&mut self) -> Result<Expr, Error> {
        let mut left = self.and()?;
        while self.at(&Tok::Or) {
            let span = self.advance().span;
            let right = self.and()?;
            left = Expr::Or(Box::new(left), Box::new(right), span);
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, Error> {
        let mut left = self.not()?;
        while self.at(&Tok::And) {
            let span = self.advance().span;
            let right = self.not()?;
            left = Expr::And(Box::new(left), Box::new(right), span);
        }
        Ok(left)
    }

    fn not(&mut self) -> Result<Expr, Error> {
        if self.at(&Tok::Not) {
            let span = self.advance().span;
            return Ok(Expr::Not(Box::new(self.not()?), span));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Expr, Error> {
        let left = self.additive()?;
        let op = match self.peek() {
            Tok::Eq => BinOp::Eq,
            Tok::Ne => BinOp::Ne,
            Tok::Lt => BinOp::Lt,
            Tok::Le => BinOp::Le,
            Tok::Gt => BinOp::Gt,
            Tok::Ge => BinOp::Ge,
            _ => return Ok(left),
        };
        let span = self.advance().span;
        let right = self.additive()?;
        if matches!(self.peek(), Tok::Eq | Tok::Ne | Tok::Lt | Tok::Le | Tok::Gt | Tok::Ge) {
            return Err(Error::syntax("comparisons cannot be chained; use `and`", self.span()));
        }
        Ok(Expr::Binary(op, Box::new(left), Box::new(right), span))
    }

    fn additive(&mut self) -> Result<Expr, Error> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => return Ok(left),
            };
            let span = self.advance().span;
            let right = self.multiplicative()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right), span);
        }
    }

    fn multiplicative(&mut self) -> Result<Expr, Error> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Mod,
                _ => return Ok(left),
            };
            let span = self.advance().span;
            let right = self.unary()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right), span);
        }
    }

    fn unary(&mut self) -> Result<Expr, Error> {
        if self.at(&Tok::Minus) {
            let span = self.advance().span;
            return Ok(Expr::Neg(Box::new(self.unary()?), span));
        }
        if self.at(&Tok::Await) {
            let span = self.advance().span;
            let effect = self.ident("an effect name after `await`, like `await sleep(1)`")?;
            if !self.at(&Tok::LParen) {
                return Err(self.unexpected("`(`: `await` takes an effect call, like `await sleep(1)`"));
            }
            self.advance();
            let args = self.arguments()?;
            return Ok(Expr::Await { effect, args, span });
        }
        self.postfix()
    }

    fn arguments(&mut self) -> Result<Vec<Expr>, Error> {
        let mut args = Vec::new();
        while !self.at(&Tok::RParen) {
            args.push(self.expr()?);
            if !self.eat(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen, "`)` or `,`")?;
        if args.len() > 255 {
            return Err(Error::syntax("at most 255 arguments", self.span()));
        }
        Ok(args)
    }

    fn postfix(&mut self) -> Result<Expr, Error> {
        let mut e = self.primary()?;
        loop {
            match self.peek() {
                Tok::LParen => {
                    let span = self.advance().span;
                    let args = self.arguments()?;
                    e = Expr::Call(Box::new(e), args, span);
                }
                Tok::LBracket => {
                    let span = self.advance().span;
                    let index = self.expr()?;
                    self.expect(&Tok::RBracket, "`]`")?;
                    e = Expr::Index(Box::new(e), Box::new(index), span);
                }
                Tok::Dot => {
                    let span = self.advance().span;
                    let name = self.ident("a field name")?;
                    e = Expr::Field(Box::new(e), name, span);
                }
                _ => return Ok(e),
            }
        }
    }

    fn primary(&mut self) -> Result<Expr, Error> {
        let span = self.span();
        match self.peek().clone() {
            Tok::Int(n) => {
                self.advance();
                Ok(Expr::Int(n, span))
            }
            Tok::Float(x) => {
                self.advance();
                Ok(Expr::Float(x, span))
            }
            Tok::True => {
                self.advance();
                Ok(Expr::Bool(true, span))
            }
            Tok::False => {
                self.advance();
                Ok(Expr::Bool(false, span))
            }
            Tok::Nil => {
                self.advance();
                Ok(Expr::Nil(span))
            }
            Tok::Str(parts) => {
                self.advance();
                string(parts, span)
            }
            Tok::Ident(name) => {
                self.advance();
                Ok(Expr::Var(name, span))
            }
            Tok::LParen => {
                self.advance();
                let e = self.expr()?;
                self.expect(&Tok::RParen, "`)`")?;
                Ok(e)
            }
            Tok::LBracket => {
                self.advance();
                let mut items = Vec::new();
                loop {
                    self.skip_semis();
                    if self.at(&Tok::RBracket) {
                        break;
                    }
                    items.push(self.expr()?);
                    self.skip_semis();
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.skip_semis();
                self.expect(&Tok::RBracket, "`]` or `,`")?;
                Ok(Expr::List(items, span))
            }
            Tok::LBrace => {
                self.advance();
                let mut entries: Vec<(String, Expr)> = Vec::new();
                loop {
                    self.skip_semis();
                    if self.at(&Tok::RBrace) {
                        break;
                    }
                    let kspan = self.span();
                    let key = match self.peek().clone() {
                        Tok::Ident(k) => k,
                        Tok::Str(parts) => match parts.as_slice() {
                            [StrPart::Lit(k)] => k.clone(),
                            _ => return Err(Error::syntax("map keys cannot be interpolated", kspan)),
                        },
                        _ => return Err(self.unexpected("a key")),
                    };
                    self.advance();
                    if entries.iter().any(|(k, _)| *k == key) {
                        return Err(Error::syntax(format!("duplicate key `{key}`"), kspan));
                    }
                    self.expect(&Tok::Colon, "`:`")?;
                    entries.push((key, self.expr()?));
                    self.skip_semis();
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.skip_semis();
                self.expect(&Tok::RBrace, "`}` or `,`")?;
                Ok(Expr::Map(entries, span))
            }
            Tok::Fn => {
                self.advance();
                Ok(Expr::Lambda(Box::new(self.function("<fn>".into(), span)?)))
            }
            _ => Err(self.unexpected("an expression")),
        }
    }
}

fn string(parts: Vec<StrPart>, span: Span) -> Result<Expr, Error> {
    if let [StrPart::Lit(s)] = parts.as_slice() {
        return Ok(Expr::Str(s.clone(), span));
    }
    let mut exprs = Vec::new();
    for part in parts {
        match part {
            StrPart::Lit(s) => exprs.push(Expr::Str(s, span)),
            StrPart::Expr(src, at) => {
                let mut p = Parser {
                    toks: lex_at(&src, at)?,
                    pos: 0,
                };
                let e = p.expr()?;
                p.skip_semis();
                if !p.at(&Tok::Eof) {
                    return Err(p.unexpected("`}`"));
                }
                exprs.push(e);
            }
        }
    }
    Ok(Expr::Interp(exprs, span))
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Int(n) => n.to_string(),
        Tok::Float(x) => x.to_string(),
        Tok::Str(_) => "string".into(),
        Tok::Ident(s) => s.clone(),
        other => {
            let s = format!("{other:?}");
            match s.as_str() {
                "LParen" => "(".into(),
                "RParen" => ")".into(),
                "LBracket" => "[".into(),
                "RBracket" => "]".into(),
                "LBrace" => "{".into(),
                "RBrace" => "}".into(),
                "Comma" => ",".into(),
                "Dot" => ".".into(),
                "Colon" => ":".into(),
                "Plus" => "+".into(),
                "Minus" => "-".into(),
                "Star" => "*".into(),
                "Slash" => "/".into(),
                "Percent" => "%".into(),
                "Assign" => "=".into(),
                "Eq" => "==".into(),
                "Ne" => "!=".into(),
                "Lt" => "<".into(),
                "Le" => "<=".into(),
                "Gt" => ">".into(),
                "Ge" => ">=".into(),
                "PlusAssign" => "+=".into(),
                "MinusAssign" => "-=".into(),
                "StarAssign" => "*=".into(),
                "SlashAssign" => "/=".into(),
                _ => s.to_lowercase(),
            }
        }
    }
}
