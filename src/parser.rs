use std::error::Error;

use crate::ast::{BinOp, Block, CallArg, Expr, Stmt, StrPiece, UnOp};
use crate::error::{FirelangError, Span};
use crate::lexer::{SpannedToken, StrPart, Token};

type ParsedArgs = Vec<CallArg>;

/// 二項演算子の優先順位表(弱い順)。`parse_or` / `parse_and` より強く、
/// 単項より弱い 3 段。和・積の各段は左結合。
const BINARY_LEVELS: &[&[(Token, BinOp)]] = &[
    &[
        (Token::EqEq, BinOp::Eq),
        (Token::Ne, BinOp::Ne),
        (Token::Gt, BinOp::Gt),
        (Token::Lt, BinOp::Lt),
        (Token::Ge, BinOp::Ge),
        (Token::Le, BinOp::Le),
    ],
    &[(Token::Plus, BinOp::Add), (Token::Minus, BinOp::Sub)],
    &[(Token::Star, BinOp::Mul), (Token::Slash, BinOp::Div)],
];

pub struct Parser {
    tokens: Vec<Token>,
    spans: Vec<Option<Span>>,
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
        let spans = vec![None; tokens.len()];
        Self {
            tokens,
            spans,
            pos: 0,
            suppress_block_calls: 0,
            depth: 0,
        }
    }

    pub fn from_spanned(spanned: Vec<SpannedToken>) -> Self {
        let mut tokens = Vec::with_capacity(spanned.len());
        let mut spans = Vec::with_capacity(spanned.len());
        for s in spanned {
            tokens.push(s.token);
            spans.push(Some(s.span));
        }
        Self {
            tokens,
            spans,
            pos: 0,
            suppress_block_calls: 0,
            depth: 0,
        }
    }
    pub fn parse_program(&mut self) -> Result<Vec<Stmt>, Box<dyn Error>> {
        Ok(self.parse_program_with_spans()?.0)
    }

    /// トップレベル文と各文の開始位置を返す。runtime のエラーに文単位の
    /// 位置を付与するために使う。
    pub fn parse_program_with_spans(&mut self) -> Result<(Vec<Stmt>, Vec<Span>), Box<dyn Error>> {
        let mut out = Vec::new();
        let mut spans = Vec::new();
        self.skip_separators();
        while !self.at(&Token::Eof) {
            spans.push(self.peek_span().unwrap_or(Span::new(1, 1)));
            out.push(self.parse_stmt()?);
            self.skip_separators();
        }
        Ok((out, spans))
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
            let parsed = self.parse_comma_separated(Token::RParen, |p| p.expect_ident());
            params = parsed?;
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
            // `else if ...` は else の直後が If なら再帰的にパースし、
            // else_body に1文だけ入れる。`elsif` は増やさない。
            if self.at(&Token::If) {
                else_body = vec![self.parse_if()?];
            } else {
                else_body = self.parse_brace_block()?;
            }
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
        self.parse_or()
    }
    fn parse_or(&mut self) -> Result<Expr, Box<dyn Error>> {
        let mut left = self.parse_and()?;
        while self.at(&Token::OrOr) {
            self.bump();
            let right = self.parse_and()?;
            left = Expr::Binary {
                left: Box::new(left),
                op: BinOp::Or,
                right: Box::new(right),
            };
        }
        Ok(left)
    }
    fn parse_and(&mut self) -> Result<Expr, Box<dyn Error>> {
        let mut left = self.parse_comparison()?;
        while self.at(&Token::AndAnd) {
            self.bump();
            let right = self.parse_comparison()?;
            left = Expr::Binary {
                left: Box::new(left),
                op: BinOp::And,
                right: Box::new(right),
            };
        }
        Ok(left)
    }
    fn parse_comparison(&mut self) -> Result<Expr, Box<dyn Error>> {
        self.parse_binary_level(0)
    }
    /// 二項演算子を優先順位テーブルで一段ずつ読む。深い level ほど強い。
    /// 各 level は左結合のループで、次の level(または単項)へ再帰する。
    fn parse_binary_level(&mut self, level: usize) -> Result<Expr, Box<dyn Error>> {
        let mut left = if level + 1 < BINARY_LEVELS.len() {
            self.parse_binary_level(level + 1)?
        } else {
            self.parse_unary()?
        };
        while let Some(op) = BINARY_LEVELS[level]
            .iter()
            .find(|(t, _)| self.at(t))
            .map(|(_, op)| *op)
        {
            self.bump();
            let right = if level + 1 < BINARY_LEVELS.len() {
                self.parse_binary_level(level + 1)?
            } else {
                self.parse_unary()?
            };
            left = Expr::Binary {
                left: Box::new(left),
                op,
                right: Box::new(right),
            }
        }
        Ok(left)
    }
    fn parse_unary(&mut self) -> Result<Expr, Box<dyn Error>> {
        if self.depth > 0 {
            self.skip_newlines();
        }
        if self.at(&Token::Bang) {
            self.bump();
            let expr = self.parse_unary()?;
            return Ok(Expr::Unary {
                op: UnOp::Not,
                expr: Box::new(expr),
            });
        }
        if self.at(&Token::Minus) {
            self.bump();
            let expr = self.parse_unary()?;
            return Ok(Expr::Unary {
                op: UnOp::Neg,
                expr: Box::new(expr),
            });
        }
        self.parse_call()
    }
    fn parse_call(&mut self) -> Result<Expr, Box<dyn Error>> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.at(&Token::Dot) {
                self.bump();
                let name = self.expect_ident()?;
                let args = self.parse_args()?;
                expr = Expr::Call {
                    receiver: Some(Box::new(expr)),
                    name,
                    args,
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
                // `(` 付き・引数開始トークン・`{` ブロックのいずれかが続けば
                // 呼び出し。`parse_args` は空の引数列を返すので `LBrace` の
                // 分岐は不要(ブロックは `parse_optional_brace_block` が読む)。
                if self.at(&Token::LParen)
                    || (self.is_arg_start() && self.suppress_block_calls == 0)
                    || (self.at(&Token::LBrace) && self.suppress_block_calls == 0)
                {
                    let args = self.parse_args()?;
                    Ok(Expr::Call {
                        receiver: None,
                        name,
                        args,
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
            t => Err(self.parse_err(format!("unexpected token: {t:?}"))),
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
        self.parse_comma_separated(Token::RBrace, |p| {
            let key = match p.peek().clone() {
                Token::Str(parts) => {
                    let span = p.peek_span();
                    p.bump();
                    match parts.as_slice() {
                        [StrPart::Lit(s)] => s.clone(),
                        _ => {
                            return Err(FirelangError::parse(
                                "interpolation is not allowed in map keys",
                                span,
                            )
                            .into())
                        }
                    }
                }
                Token::Ident(s) => {
                    p.bump();
                    s
                }
                t => {
                    return Err(p.parse_err(format!(
                        "object key must be string or identifier, got {t:?}"
                    )))
                }
            };
            p.expect(Token::Colon)?;
            let value = p.parse_expr()?;
            Ok((key, value))
        })
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
    /// delimiter 内のカンマ区切り列を読む。要素前後の改行を無視し、
    /// 末尾カンマを許す。`end` 自体の消費は呼び出し側が行う。
    fn parse_comma_separated<T>(
        &mut self,
        end: Token,
        mut item: impl FnMut(&mut Self) -> Result<T, Box<dyn Error>>,
    ) -> Result<Vec<T>, Box<dyn Error>> {
        self.skip_newlines();
        let mut out = vec![];
        if !self.at(&end) {
            loop {
                out.push(item(self)?);
                self.skip_newlines();
                if self.at(&Token::Comma) {
                    self.bump();
                    self.skip_newlines();
                    if self.at(&end) {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        Ok(out)
    }
    fn parse_array_items(&mut self) -> Result<Vec<Expr>, Box<dyn Error>> {
        self.parse_comma_separated(Token::RBracket, |p| p.parse_expr())
    }
    fn parse_arg(&mut self) -> Result<CallArg, Box<dyn Error>> {
        if let Some((name, value)) = self.parse_kwarg()? {
            Ok(CallArg::Keyword { name, value })
        } else {
            Ok(CallArg::Positional(self.parse_expr()?))
        }
    }

    // Keep positional and keyword arguments in one sequence.  The runtime
    // later separates them for parameter binding, but evaluation must retain
    // this source order.
    fn parse_args(&mut self) -> Result<ParsedArgs, Box<dyn Error>> {
        if self.at(&Token::LParen) {
            self.bump();
            self.depth += 1;
            let parsed = self.parse_comma_separated(Token::RParen, |p| p.parse_arg());
            self.depth -= 1;
            let args = parsed?;
            self.expect(Token::RParen)?;
            return Ok(args);
        }
        if self.is_arg_start() {
            let mut args = vec![];
            loop {
                args.push(self.parse_arg()?);
                if self.at(&Token::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
            return Ok(args);
        }
        Ok(vec![])
    }
    fn expr_from_string_parts(parts: Vec<StrPart>) -> Result<Expr, Box<dyn Error>> {
        if let [StrPart::Lit(s)] = parts.as_slice() {
            return Ok(Expr::Str(s.clone()));
        }
        let mut pieces = vec![];
        for part in parts {
            match part {
                StrPart::Lit(s) => pieces.push(StrPiece::Lit(s)),
                StrPart::Interp(tokens) => {
                    pieces.push(StrPiece::Expr(Self::parse_embedded_tokens(tokens)?))
                }
            }
        }
        Ok(Expr::StrInterp(pieces))
    }

    /// 補間の中身(トークン列、末尾 Eof 付き)を単一の式として読む。
    /// 前後の改行・`;` は許すが、文が 2 つ以上あるとエラー。
    fn parse_embedded_tokens(tokens: Vec<SpannedToken>) -> Result<Expr, Box<dyn Error>> {
        let mut parser = Parser::from_spanned(tokens);
        parser.skip_separators();
        let expr = parser.parse_expr()?;
        parser.skip_separators();
        if !parser.at(&Token::Eof) {
            return Err(FirelangError::parse(
                "interpolation must contain a single expression",
                None,
            )
            .into());
        }
        Ok(expr)
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
        Ok(Some(self.parse_brace_block_with_params()?.0))
    }
    fn parse_brace_block(&mut self) -> Result<Vec<Stmt>, Box<dyn Error>> {
        let (block, params_span) = self.parse_brace_block_with_params()?;
        if !block.params.is_empty() {
            return Err(
                FirelangError::parse("block parameters are not allowed here", params_span).into(),
            );
        }
        Ok(block.body)
    }
    fn parse_brace_block_with_params(&mut self) -> Result<(Block, Option<Span>), Box<dyn Error>> {
        self.expect(Token::LBrace)?;
        // `{\n|x| ... }` のようにパラメータリストの直前に改行が置けるようにする。
        self.skip_newlines();
        let params_span = self.peek_span();
        let params = self.parse_block_params()?;
        // 本文は「文の並び」なので `;` も文の区切りとして扱う。
        self.skip_separators();
        let mut body = vec![];
        while !self.at(&Token::RBrace) {
            body.push(self.parse_stmt()?);
            self.skip_separators();
        }
        self.expect(Token::RBrace)?;
        Ok((
            Block {
                params: params.clone(),
                body,
            },
            params_span.filter(|_| !params.is_empty()),
        ))
    }
    fn parse_block_params(&mut self) -> Result<Vec<String>, Box<dyn Error>> {
        // 空パラメータ `||` は lexer が OrOr 1 トークンにまとめるため、ここで吸収する。
        if self.at(&Token::OrOr) {
            self.bump();
            return Ok(vec![]);
        }
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
                | Token::Bang
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
    fn peek_span(&self) -> Option<Span> {
        self.spans.get(self.pos).and_then(|s| *s)
    }
    fn parse_err(&self, message: impl Into<String>) -> Box<dyn Error> {
        FirelangError::parse(message, self.peek_span()).into()
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
            Err(self.parse_err(format!("expected {token:?}, got {:?}", self.peek())))
        }
    }
    fn expect_ident(&mut self) -> Result<String, Box<dyn Error>> {
        match self.peek().clone() {
            Token::Ident(s) => {
                self.bump();
                Ok(s)
            }
            t => Err(self.parse_err(format!("expected ident, got {t:?}"))),
        }
    }
}
