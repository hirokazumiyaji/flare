use std::error::Error;

use crate::ast::{BinOp, Expr, Stmt};
use crate::lexer::Token;

type ParsedArgs = (Vec<Expr>, Vec<(String, Expr)>);

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }
    pub fn parse_program(&mut self) -> Result<Vec<Stmt>, Box<dyn Error>> {
        let mut out = Vec::new();
        self.skip_newlines();
        while !self.at(&Token::Eof) {
            out.push(self.parse_stmt()?);
            self.skip_newlines();
        }
        Ok(out)
    }
    fn parse_stmt(&mut self) -> Result<Stmt, Box<dyn Error>> {
        match self.peek() {
            Token::Fn => self.parse_def(),
            Token::Return => self.parse_return(),
            Token::If => self.parse_if(),
            Token::While => self.parse_while(),
            Token::Ident(name) if self.peek_n(1) == Some(&Token::Eq) => {
                let name = name.clone();
                self.bump();
                self.bump();
                Ok(Stmt::Assign {
                    name,
                    value: self.parse_expr()?,
                })
            }
            _ => Ok(Stmt::Expr(self.parse_expr()?)),
        }
    }
    fn parse_def(&mut self) -> Result<Stmt, Box<dyn Error>> {
        self.expect(Token::Fn)?;
        let name = self.expect_ident()?;
        let mut params = vec![];
        if self.at(&Token::LParen) {
            self.bump();
            if !self.at(&Token::RParen) {
                loop {
                    params.push(self.expect_ident()?);
                    if self.at(&Token::Comma) {
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
            self.expect(Token::RParen)?;
        }
        let body = self.parse_brace_block()?;
        Ok(Stmt::Def { name, params, body })
    }
    fn parse_return(&mut self) -> Result<Stmt, Box<dyn Error>> {
        self.expect(Token::Return)?;
        if matches!(self.peek(), Token::Newline | Token::RBrace | Token::Eof) {
            Ok(Stmt::Return(None))
        } else {
            Ok(Stmt::Return(Some(self.parse_expr()?)))
        }
    }
    fn parse_if(&mut self) -> Result<Stmt, Box<dyn Error>> {
        self.expect(Token::If)?;
        let cond = self.parse_expr()?;
        let then_body = self.parse_brace_block()?;
        let mut else_body = vec![];
        if self.at(&Token::Else) {
            self.bump();
            else_body = self.parse_brace_block()?;
        }
        Ok(Stmt::If {
            cond,
            then_body,
            else_body,
        })
    }
    fn parse_while(&mut self) -> Result<Stmt, Box<dyn Error>> {
        self.expect(Token::While)?;
        let cond = self.parse_expr()?;
        let body = self.parse_brace_block()?;
        Ok(Stmt::While { cond, body })
    }
    fn parse_expr(&mut self) -> Result<Expr, Box<dyn Error>> {
        self.parse_comparison()
    }
    fn parse_comparison(&mut self) -> Result<Expr, Box<dyn Error>> {
        let mut left = self.parse_add_sub()?;
        loop {
            let op = match self.peek() {
                Token::EqEq => BinOp::Eq,
                Token::Ne => BinOp::Ne,
                Token::Gt => BinOp::Gt,
                Token::Lt => BinOp::Lt,
                Token::Ge => BinOp::Ge,
                Token::Le => BinOp::Le,
                _ => break,
            };
            self.bump();
            left = Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(self.parse_add_sub()?),
            }
        }
        Ok(left)
    }
    fn parse_add_sub(&mut self) -> Result<Expr, Box<dyn Error>> {
        let mut left = self.parse_mul_div()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            left = Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(self.parse_mul_div()?),
            }
        }
        Ok(left)
    }
    fn parse_mul_div(&mut self) -> Result<Expr, Box<dyn Error>> {
        let mut left = self.parse_call()?;
        loop {
            let op = match self.peek() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                _ => break,
            };
            self.bump();
            left = Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(self.parse_call()?),
            }
        }
        Ok(left)
    }
    fn parse_call(&mut self) -> Result<Expr, Box<dyn Error>> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.at(&Token::Dot) {
                self.bump();
                let name = self.expect_ident()?;
                let (args, kwargs) = self.parse_args()?;
                expr = Expr::Call {
                    receiver: Some(Box::new(expr)),
                    name,
                    args,
                    kwargs,
                    block: self.parse_optional_brace_block()?,
                };
            } else {
                break;
            }
        }
        Ok(expr)
    }
    fn parse_primary(&mut self) -> Result<Expr, Box<dyn Error>> {
        match self.peek().clone() {
            Token::Int(v) => {
                self.bump();
                Ok(Expr::Int(v))
            }
            Token::Str(s) => {
                self.bump();
                Ok(Expr::Str(s))
            }
            Token::True => {
                self.bump();
                Ok(Expr::Bool(true))
            }
            Token::False => {
                self.bump();
                Ok(Expr::Bool(false))
            }
            Token::Nil => {
                self.bump();
                Ok(Expr::Nil)
            }
            Token::Ident(name) => {
                self.bump();
                if self.at(&Token::LParen) || self.is_arg_start() {
                    let (args, kwargs) = self.parse_args()?;
                    Ok(Expr::Call {
                        receiver: None,
                        name,
                        args,
                        kwargs,
                        block: self.parse_optional_brace_block()?,
                    })
                } else if self.at(&Token::LBrace) {
                    Ok(Expr::Call {
                        receiver: None,
                        name,
                        args: vec![],
                        kwargs: vec![],
                        block: self.parse_optional_brace_block()?,
                    })
                } else {
                    Ok(Expr::Var(name))
                }
            }
            Token::LParen => {
                self.bump();
                let out = self.parse_expr()?;
                self.expect(Token::RParen)?;
                Ok(out)
            }
            Token::LBrace => self.parse_map_literal(),
            Token::LBracket => self.parse_array_literal(),
            t => Err(format!("unexpected token: {t:?}").into()),
        }
    }
    fn parse_map_literal(&mut self) -> Result<Expr, Box<dyn Error>> {
        self.expect(Token::LBrace)?;
        let mut entries = vec![];
        if !self.at(&Token::RBrace) {
            loop {
                let key = match self.peek().clone() {
                    Token::Str(s) | Token::Ident(s) => {
                        self.bump();
                        s
                    }
                    t => {
                        return Err(
                            format!("object key must be string or identifier, got {t:?}").into(),
                        )
                    }
                };
                self.expect(Token::Colon)?;
                let value = self.parse_expr()?;
                entries.push((key, value));
                if self.at(&Token::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.expect(Token::RBrace)?;
        Ok(Expr::Map(entries))
    }
    fn parse_array_literal(&mut self) -> Result<Expr, Box<dyn Error>> {
        self.expect(Token::LBracket)?;
        let mut items = vec![];
        if !self.at(&Token::RBracket) {
            loop {
                items.push(self.parse_expr()?);
                if self.at(&Token::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.expect(Token::RBracket)?;
        Ok(Expr::Array(items))
    }
    fn parse_arg_into(
        &mut self,
        args: &mut Vec<Expr>,
        kwargs: &mut Vec<(String, Expr)>,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(kw) = self.parse_kwarg()? {
            kwargs.push(kw);
        } else {
            args.push(self.parse_expr()?);
        }
        Ok(())
    }

    fn parse_args(&mut self) -> Result<ParsedArgs, Box<dyn Error>> {
        let mut args = vec![];
        let mut kwargs = vec![];
        if self.at(&Token::LParen) {
            self.bump();
            if !self.at(&Token::RParen) {
                loop {
                    self.parse_arg_into(&mut args, &mut kwargs)?;
                    if self.at(&Token::Comma) {
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
            self.expect(Token::RParen)?;
            return Ok((args, kwargs));
        }
        if self.is_arg_start() {
            loop {
                self.parse_arg_into(&mut args, &mut kwargs)?;
                if self.at(&Token::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        Ok((args, kwargs))
    }
    fn parse_kwarg(&mut self) -> Result<Option<(String, Expr)>, Box<dyn Error>> {
        if let Token::Ident(name) = self.peek().clone() {
            if self.peek_n(1) == Some(&Token::Colon) {
                self.bump();
                self.bump();
                let value = self.parse_expr()?;
                return Ok(Some((name, value)));
            }
        }
        Ok(None)
    }
    fn parse_optional_brace_block(&mut self) -> Result<Option<Vec<Stmt>>, Box<dyn Error>> {
        if !self.at(&Token::LBrace) {
            return Ok(None);
        }
        Ok(Some(self.parse_brace_block()?))
    }
    fn parse_brace_block(&mut self) -> Result<Vec<Stmt>, Box<dyn Error>> {
        self.expect(Token::LBrace)?;
        self.skip_newlines();
        let mut body = vec![];
        while !self.at(&Token::RBrace) {
            body.push(self.parse_stmt()?);
            self.skip_newlines();
        }
        self.expect(Token::RBrace)?;
        Ok(body)
    }
    fn is_arg_start(&self) -> bool {
        matches!(
            self.peek(),
            Token::Int(_)
                | Token::Str(_)
                | Token::Ident(_)
                | Token::True
                | Token::False
                | Token::Nil
                | Token::LParen
                | Token::LBracket
        )
    }
    fn skip_newlines(&mut self) {
        while self.at(&Token::Newline) {
            self.bump();
        }
    }
    fn at(&self, token: &Token) -> bool {
        self.peek() == token
    }
    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }
    fn peek_n(&self, n: usize) -> Option<&Token> {
        self.tokens.get(self.pos + n)
    }
    fn bump(&mut self) {
        self.pos += 1;
    }
    fn expect(&mut self, token: Token) -> Result<(), Box<dyn Error>> {
        if self.peek() == &token {
            self.bump();
            Ok(())
        } else {
            Err(format!("expected {token:?}, got {:?}", self.peek()).into())
        }
    }
    fn expect_ident(&mut self) -> Result<String, Box<dyn Error>> {
        match self.peek().clone() {
            Token::Ident(s) => {
                self.bump();
                Ok(s)
            }
            t => Err(format!("expected ident, got {t:?}").into()),
        }
    }
}
