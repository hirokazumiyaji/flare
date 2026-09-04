use std::error::Error;

use crate::ast::{BinOp, Block, Expr, Stmt, StrPiece};
use crate::lexer::{self, StrPart, Token};

type ParsedArgs = (Vec<Expr>, Vec<(String, Expr)>);

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// if / while の条件式パース中に 1 以上。条件式内ではメソッド呼び出しの
    /// 省略可能ブロック(`obj.method { ... }`)を消費しない(制御構文の本体
    /// `{ ... }` と区別するため)。
    suppress_block_calls: usize,
    /// マップ / 配列リテラル・引数リスト・グループ化・添字・仮引数リストの
    /// 内側で 1 以上。内側では改行をトークンとして透過的に無視する。
    depth: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            pos: 0,
            suppress_block_calls: 0,
            depth: 0,
        }
    }
    pub fn parse_program(&mut self) -> Result<Vec<Stmt>, Box<dyn Error>> {
        let mut out = Vec::new();
        self.skip_separators();
        while !self.at(&Token::Eof) {
            out.push(self.parse_stmt()?);
            self.skip_separators();
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
            self.skip_newlines();
            if !self.at(&Token::RParen) {
                loop {
                    params.push(self.expect_ident()?);
                    self.skip_newlines();
                    if self.at(&Token::Comma) {
                        self.bump();
                        self.skip_newlines();
                        if self.at(&Token::RParen) {
                            break;
                        }
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
        if matches!(
            self.peek(),
            Token::Newline | Token::Semicolon | Token::RBrace | Token::Eof
        ) {
            Ok(Stmt::Return(None))
        } else {
            Ok(Stmt::Return(Some(self.parse_expr()?)))
        }
    }
    fn parse_if(&mut self) -> Result<Stmt, Box<dyn Error>> {
        self.expect(Token::If)?;
        self.suppress_block_calls += 1;
        let cond = self.parse_expr();
        self.suppress_block_calls -= 1;
        let cond = cond?;
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
        self.suppress_block_calls += 1;
        let cond = self.parse_expr();
        self.suppress_block_calls -= 1;
        let cond = cond?;
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
            } else if self.at(&Token::LBracket) {
                self.bump();
                self.depth += 1;
                self.skip_newlines();
                let index = self.parse_expr();
                self.depth -= 1;
                let index = index?;
                self.skip_newlines();
                self.expect(Token::RBracket)?;
                expr = Expr::Index {
                    base: Box::new(expr),
                    index: Box::new(index),
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
            Token::Str(parts) => {
                self.bump();
                Self::expr_from_string_parts(parts)
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
                if self.at(&Token::LParen)
                    || (self.is_arg_start() && self.suppress_block_calls == 0)
                {
                    let (args, kwargs) = self.parse_args()?;
                    Ok(Expr::Call {
                        receiver: None,
                        name,
                        args,
                        kwargs,
                        block: self.parse_optional_brace_block()?,
                    })
                } else if self.at(&Token::LBrace) && self.suppress_block_calls == 0 {
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
                self.depth += 1;
                self.skip_newlines();
                let out = self.parse_expr();
                self.depth -= 1;
                let out = out?;
                self.skip_newlines();
                self.expect(Token::RParen)?;
                Ok(out)
            }
            Token::LBrace => self.parse_map_literal(),
            Token::LBracket => self.parse_array_literal(),
            Token::Newline if self.depth > 0 => {
                // delimiter の内側では改行を透過的に無視する(式の途中を含む)。
                self.bump();
                self.parse_primary()
            }
            t => Err(format!("unexpected token: {t:?}").into()),
        }
    }
    fn parse_map_literal(&mut self) -> Result<Expr, Box<dyn Error>> {
        self.expect(Token::LBrace)?;
        self.depth += 1;
        let entries = self.parse_map_entries();
        self.depth -= 1;
        let entries = entries?;
        self.expect(Token::RBrace)?;
        Ok(Expr::Map(entries))
    }
    fn parse_map_entries(&mut self) -> Result<Vec<(String, Expr)>, Box<dyn Error>> {
        self.skip_newlines();
        let mut entries = vec![];
        if !self.at(&Token::RBrace) {
            loop {
                let key = match self.peek().clone() {
                    Token::Str(parts) => {
                        self.bump();
                        match parts.as_slice() {
                            [lexer::StrPart::Lit(s)] => s.clone(),
                            _ => return Err("interpolation is not allowed in map keys".into()),
                        }
                    }
                    Token::Ident(s) => {
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
                self.skip_newlines();
                if self.at(&Token::Comma) {
                    self.bump();
                    self.skip_newlines();
                    if self.at(&Token::RBrace) {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        Ok(entries)
    }
    fn parse_array_literal(&mut self) -> Result<Expr, Box<dyn Error>> {
        self.expect(Token::LBracket)?;
        self.depth += 1;
        let items = self.parse_array_items();
        self.depth -= 1;
        let items = items?;
        self.expect(Token::RBracket)?;
        Ok(Expr::Array(items))
    }
    fn parse_array_items(&mut self) -> Result<Vec<Expr>, Box<dyn Error>> {
        self.skip_newlines();
        let mut items = vec![];
        if !self.at(&Token::RBracket) {
            loop {
                items.push(self.parse_expr()?);
                self.skip_newlines();
                if self.at(&Token::Comma) {
                    self.bump();
                    self.skip_newlines();
                    if self.at(&Token::RBracket) {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        Ok(items)
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
            self.depth += 1;
            let parsed = self.parse_paren_args(&mut args, &mut kwargs);
            self.depth -= 1;
            parsed?;
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

    fn parse_paren_args(
        &mut self,
        args: &mut Vec<Expr>,
        kwargs: &mut Vec<(String, Expr)>,
    ) -> Result<(), Box<dyn Error>> {
        self.skip_newlines();
        if !self.at(&Token::RParen) {
            loop {
                self.parse_arg_into(args, kwargs)?;
                self.skip_newlines();
                if self.at(&Token::Comma) {
                    self.bump();
                    self.skip_newlines();
                    if self.at(&Token::RParen) {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        Ok(())
    }
    fn expr_from_string_parts(parts: Vec<StrPart>) -> Result<Expr, Box<dyn Error>> {
        if let [StrPart::Lit(s)] = parts.as_slice() {
            return Ok(Expr::Str(s.clone()));
        }
        let mut pieces = vec![];
        for part in parts {
            match part {
                StrPart::Lit(s) => pieces.push(StrPiece::Lit(s)),
                StrPart::Interp(src) => {
                    pieces.push(StrPiece::Expr(Self::parse_embedded_expr(&src)?))
                }
            }
        }
        Ok(Expr::StrInterp(pieces))
    }

    fn parse_embedded_expr(src: &str) -> Result<Expr, Box<dyn Error>> {
        let tokens = lexer::lex(src)?;
        let program = Parser::new(tokens).parse_program()?;
        match program.as_slice() {
            [Stmt::Expr(e)] => Ok(e.clone()),
            _ => Err("interpolation must contain a single expression".into()),
        }
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
    fn parse_optional_brace_block(&mut self) -> Result<Option<Block>, Box<dyn Error>> {
        if !self.at(&Token::LBrace) || self.suppress_block_calls > 0 {
            return Ok(None);
        }
        Ok(Some(self.parse_brace_block_with_params()?))
    }
    fn parse_brace_block(&mut self) -> Result<Vec<Stmt>, Box<dyn Error>> {
        let block = self.parse_brace_block_with_params()?;
        if !block.params.is_empty() {
            return Err("block parameters are not allowed here".into());
        }
        Ok(block.body)
    }
    fn parse_brace_block_with_params(&mut self) -> Result<Block, Box<dyn Error>> {
        self.expect(Token::LBrace)?;
        // `{\n|x| ... }` のようにパラメータリストの直前に改行が置けるようにする。
        self.skip_newlines();
        let params = self.parse_block_params()?;
        // 本文は「文の並び」なので `;` も文の区切りとして扱う。
        self.skip_separators();
        let mut body = vec![];
        while !self.at(&Token::RBrace) {
            body.push(self.parse_stmt()?);
            self.skip_separators();
        }
        self.expect(Token::RBrace)?;
        Ok(Block { params, body })
    }
    fn parse_block_params(&mut self) -> Result<Vec<String>, Box<dyn Error>> {
        if !self.at(&Token::Pipe) {
            return Ok(vec![]);
        }
        self.bump();
        self.skip_newlines();
        let mut params = vec![];
        if self.at(&Token::Pipe) {
            self.bump();
            return Ok(params);
        }
        loop {
            params.push(self.expect_ident()?);
            self.skip_newlines();
            if self.at(&Token::Comma) {
                self.bump();
                self.skip_newlines();
                if self.at(&Token::Pipe) {
                    break;
                }
            } else {
                break;
            }
        }
        self.expect(Token::Pipe)?;
        Ok(params)
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
        )
    }
    /// 文の区切り(改行または `;`)をスキップする。ブロック本文・プログラム
    /// トップレベルなど「文の並び」の境界で使う。
    fn skip_separators(&mut self) {
        while self.at(&Token::Newline) || self.at(&Token::Semicolon) {
            self.bump();
        }
    }
    /// delimiter(リテラル・引数リスト・仮引数リスト等)の内側の改行だけを
    /// スキップする。`;` は文の区切りのままなのでスキップしない(エラーになる)。
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
