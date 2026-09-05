use std::error::Error;

use crate::error::{FirelangError, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    Lit(String),
    /// `#{ ... }` の中身のトークン列(末尾 Eof 付き)。文字列として切り出さず
    /// トークン単位で読むため、中の文字列リテラル内の `{` `}` に惑わされない。
    Interp(Vec<SpannedToken>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Int(i64),
    Str(Vec<StrPart>),
    Ident(String),
    True,
    False,
    Nil,
    Fn,
    Return,
    If,
    Else,
    While,
    Plus,
    Minus,
    Star,
    Slash,
    Eq,
    EqEq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
    AndAnd,
    OrOr,
    Bang,
    LParen,
    RParen,
    Comma,
    Dot,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Pipe,
    Semicolon,
    Colon,
    Newline,
    Eof,
}

/// Token with its start position (1-based line/col).
#[derive(Debug, Clone, PartialEq)]
pub struct SpannedToken {
    pub token: Token,
    pub span: Span,
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    line: usize,
    col: usize,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            chars: input.chars().peekable(),
            line: 1,
            col: 1,
        }
    }

    fn span(&self) -> Span {
        Span::new(self.line, self.col)
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.chars.next()?;
        if ch == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(ch)
    }

    fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }

    fn spanned(&self, token: Token, span: Span) -> SpannedToken {
        SpannedToken { token, span }
    }

    /// `=` `!` `>` `<` の後に `=` が続けば 2 文字トークン。
    fn suffixed_eq(&mut self, span: Span, doubled: Token, single: Token) -> SpannedToken {
        if self.peek() == Some('=') {
            self.bump();
            self.spanned(doubled, span)
        } else {
            self.spanned(single, span)
        }
    }

    /// 入力からトークンを 1 つ読む。空白・コメントは読み飛ばす。
    /// EOF に達したら `None` を返す。
    fn next_token(&mut self) -> Result<Option<SpannedToken>, Box<dyn Error>> {
        loop {
            let Some(c) = self.peek() else {
                return Ok(None);
            };
            match c {
                ' ' | '\t' | '\r' => {
                    self.bump();
                }
                '\n' => {
                    let span = self.span();
                    self.bump();
                    return Ok(Some(self.spanned(Token::Newline, span)));
                }
                ';' => {
                    let span = self.span();
                    self.bump();
                    return Ok(Some(self.spanned(Token::Semicolon, span)));
                }
                '#' => loop {
                    match self.peek() {
                        // コメント終端の改行は Newline トークンとして残す。
                        Some('\n') => {
                            let span = self.span();
                            self.bump();
                            return Ok(Some(self.spanned(Token::Newline, span)));
                        }
                        Some(_) => {
                            self.bump();
                        }
                        None => return Ok(None),
                    }
                },
                '0'..='9' => {
                    let span = self.span();
                    let mut s = String::new();
                    while let Some(d) = self.peek() {
                        if d.is_ascii_digit() {
                            s.push(d);
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    match s.parse::<i64>() {
                        Ok(v) => return Ok(Some(self.spanned(Token::Int(v), span))),
                        Err(_) => {
                            return Err(FirelangError::lex(
                                format!("integer literal out of range: {s}"),
                                span.line,
                                span.col,
                            )
                            .into());
                        }
                    }
                }
                '"' => {
                    let span = self.span();
                    self.bump();
                    let token = self.lex_string_token()?;
                    return Ok(Some(self.spanned(token, span)));
                }
                '(' | ')' | ',' | '{' | '}' | '[' | ']' | '.' | ':' | '+' | '-' | '*' | '/' => {
                    let span = self.span();
                    self.bump();
                    // or-pattern が single_char_token の定義域と一致するため、
                    // None は到達不能。 Producer/consumer を二重化しないため
                    // テーブル側を正とし、万一のずれはエラーに落とす。
                    match single_char_token(c) {
                        Some(t) => return Ok(Some(self.spanned(t, span))),
                        None => {
                            return Err(FirelangError::lex(
                                format!("unexpected char: {c}"),
                                span.line,
                                span.col,
                            )
                            .into());
                        }
                    }
                }
                '=' => {
                    let span = self.span();
                    self.bump();
                    return Ok(Some(self.suffixed_eq(span, Token::EqEq, Token::Eq)));
                }
                '!' => {
                    let span = self.span();
                    self.bump();
                    return Ok(Some(self.suffixed_eq(span, Token::Ne, Token::Bang)));
                }
                '&' => {
                    let span = self.span();
                    self.bump();
                    if self.peek() == Some('&') {
                        self.bump();
                        return Ok(Some(self.spanned(Token::AndAnd, span)));
                    }
                    return Err(
                        FirelangError::lex("unexpected char: &", span.line, span.col).into(),
                    );
                }
                '|' => {
                    let span = self.span();
                    self.bump();
                    if self.peek() == Some('|') {
                        self.bump();
                        return Ok(Some(self.spanned(Token::OrOr, span)));
                    }
                    return Ok(Some(self.spanned(Token::Pipe, span)));
                }
                '>' => {
                    let span = self.span();
                    self.bump();
                    return Ok(Some(self.suffixed_eq(span, Token::Ge, Token::Gt)));
                }
                '<' => {
                    let span = self.span();
                    self.bump();
                    return Ok(Some(self.suffixed_eq(span, Token::Le, Token::Lt)));
                }
                _ if is_ident_start(c) => {
                    let span = self.span();
                    let mut s = String::new();
                    while let Some(ch) = self.peek() {
                        if is_ident_continue(ch) {
                            s.push(ch);
                            self.bump();
                        } else {
                            break;
                        }
                    }
                    let t = match s.as_str() {
                        "fn" => Token::Fn,
                        "return" => Token::Return,
                        "if" => Token::If,
                        "else" => Token::Else,
                        "while" => Token::While,
                        "true" => Token::True,
                        "false" => Token::False,
                        "nil" => Token::Nil,
                        _ => Token::Ident(s),
                    };
                    return Ok(Some(self.spanned(t, span)));
                }
                _ => {
                    let span = self.span();
                    return Err(FirelangError::lex(
                        format!("unexpected char: {c}"),
                        span.line,
                        span.col,
                    )
                    .into());
                }
            }
        }
    }

    /// 文字列リテラルの中身を読む。開始 `"` は消費済みであること。
    fn lex_string_token(&mut self) -> Result<Token, Box<dyn Error>> {
        let mut parts: Vec<StrPart> = vec![];
        let mut lit = String::new();
        loop {
            let ch = self.bump().ok_or_else(|| {
                FirelangError::lex("unterminated string literal", self.line, self.col)
            })?;
            match ch {
                '"' => break,
                '\\' => {
                    let esc = self.bump().ok_or_else(|| {
                        FirelangError::lex("unterminated string literal", self.line, self.col)
                    })?;
                    lit.push(match esc {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        '"' => '"',
                        '\\' => '\\',
                        '#' => '#',
                        other => other,
                    });
                }
                '#' if self.peek() == Some('{') => {
                    self.bump();
                    if !lit.is_empty() {
                        parts.push(StrPart::Lit(std::mem::take(&mut lit)));
                    }
                    parts.push(StrPart::Interp(self.lex_interp_tokens()?));
                }
                other => lit.push(other),
            }
        }
        if !lit.is_empty() || parts.is_empty() {
            parts.push(StrPart::Lit(lit));
        }
        Ok(Token::Str(parts))
    }

    /// `#{` の直後から対応する `}` までをトークン列として切り出す。
    /// トークン単位で `{` `}` を数えるため、中の文字列リテラル内の
    /// 括弧に惑わされない。末尾に Eof を付けて返す。
    fn lex_interp_tokens(&mut self) -> Result<Vec<SpannedToken>, Box<dyn Error>> {
        let mut out = vec![];
        let mut depth = 0usize;
        loop {
            let Some(st) = self.next_token()? else {
                return Err(FirelangError::lex(
                    "unterminated string interpolation",
                    self.line,
                    self.col,
                )
                .into());
            };
            match &st.token {
                Token::LBrace => {
                    depth += 1;
                    out.push(st);
                }
                Token::RBrace if depth == 0 => {
                    out.push(SpannedToken {
                        token: Token::Eof,
                        span: st.span,
                    });
                    return Ok(out);
                }
                Token::RBrace => {
                    depth -= 1;
                    out.push(st);
                }
                _ => out.push(st),
            }
        }
    }
}

pub fn lex(input: &str) -> Result<Vec<Token>, Box<dyn Error>> {
    Ok(lex_spanned(input)?.into_iter().map(|s| s.token).collect())
}

pub fn lex_spanned(input: &str) -> Result<Vec<SpannedToken>, Box<dyn Error>> {
    let mut lx = Lexer::new(input);
    let mut tokens = Vec::new();
    while let Some(st) = lx.next_token()? {
        tokens.push(st);
    }
    let span = lx.span();
    tokens.push(SpannedToken {
        token: Token::Eof,
        span,
    });
    Ok(tokens)
}

fn single_char_token(c: char) -> Option<Token> {
    Some(match c {
        '(' => Token::LParen,
        ')' => Token::RParen,
        ',' => Token::Comma,
        '{' => Token::LBrace,
        '}' => Token::RBrace,
        '[' => Token::LBracket,
        ']' => Token::RBracket,
        '.' => Token::Dot,
        ':' => Token::Colon,
        '+' => Token::Plus,
        '-' => Token::Minus,
        '*' => Token::Star,
        '/' => Token::Slash,
        _ => return None,
    })
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}
