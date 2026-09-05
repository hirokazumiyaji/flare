use std::error::Error;

use crate::error::{FirelangError, Span};

#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    Lit(String),
    /// `#{ ... }` の中身の生ソース。パースは parser が行う。
    Interp(String),
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
}

pub fn lex(input: &str) -> Result<Vec<Token>, Box<dyn Error>> {
    Ok(lex_spanned(input)?.into_iter().map(|s| s.token).collect())
}

pub fn lex_spanned(input: &str) -> Result<Vec<SpannedToken>, Box<dyn Error>> {
    let mut lx = Lexer::new(input);
    let mut tokens = Vec::new();
    let mut push = |token: Token, span: Span| {
        tokens.push(SpannedToken { token, span });
    };

    while let Some(c) = lx.peek() {
        match c {
            ' ' | '\t' | '\r' => {
                lx.bump();
            }
            '\n' => {
                let span = lx.span();
                lx.bump();
                push(Token::Newline, span);
            }
            ';' => {
                let span = lx.span();
                lx.bump();
                push(Token::Semicolon, span);
            }
            '#' => {
                while let Some(cc) = lx.peek() {
                    // コメント終端の改行は Newline トークンとして残す。
                    if cc == '\n' {
                        let span = lx.span();
                        lx.bump();
                        push(Token::Newline, span);
                        break;
                    }
                    lx.bump();
                }
            }
            '0'..='9' => {
                let span = lx.span();
                let mut s = String::new();
                while let Some(d) = lx.peek() {
                    if d.is_ascii_digit() {
                        s.push(d);
                        lx.bump();
                    } else {
                        break;
                    }
                }
                match s.parse::<i64>() {
                    Ok(v) => push(Token::Int(v), span),
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
                let span = lx.span();
                lx.bump();
                let mut parts: Vec<StrPart> = vec![];
                let mut lit = String::new();
                loop {
                    let ch = lx.bump().ok_or_else(|| {
                        FirelangError::lex("unterminated string literal", lx.line, lx.col)
                    })?;
                    match ch {
                        '"' => break,
                        '\\' => {
                            let esc = lx.bump().ok_or_else(|| {
                                FirelangError::lex("unterminated string literal", lx.line, lx.col)
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
                        '#' if lx.peek() == Some('{') => {
                            lx.bump();
                            if !lit.is_empty() {
                                parts.push(StrPart::Lit(std::mem::take(&mut lit)));
                            }
                            let mut depth = 1;
                            let mut expr = String::new();
                            loop {
                                let ch = lx.bump().ok_or_else(|| {
                                    FirelangError::lex(
                                        "unterminated string interpolation",
                                        lx.line,
                                        lx.col,
                                    )
                                })?;
                                match ch {
                                    '{' => {
                                        depth += 1;
                                        expr.push('{');
                                    }
                                    '}' => {
                                        depth -= 1;
                                        if depth == 0 {
                                            break;
                                        }
                                        expr.push('}');
                                    }
                                    other => expr.push(other),
                                }
                            }
                            parts.push(StrPart::Interp(expr));
                        }
                        other => lit.push(other),
                    }
                }
                if !lit.is_empty() || parts.is_empty() {
                    parts.push(StrPart::Lit(lit));
                }
                push(Token::Str(parts), span);
            }
            '(' => {
                let span = lx.span();
                lx.bump();
                push(Token::LParen, span);
            }
            ')' => {
                let span = lx.span();
                lx.bump();
                push(Token::RParen, span);
            }
            ',' => {
                let span = lx.span();
                lx.bump();
                push(Token::Comma, span);
            }
            '{' => {
                let span = lx.span();
                lx.bump();
                push(Token::LBrace, span);
            }
            '}' => {
                let span = lx.span();
                lx.bump();
                push(Token::RBrace, span);
            }
            '[' => {
                let span = lx.span();
                lx.bump();
                push(Token::LBracket, span);
            }
            ']' => {
                let span = lx.span();
                lx.bump();
                push(Token::RBracket, span);
            }
            '.' => {
                let span = lx.span();
                lx.bump();
                push(Token::Dot, span);
            }
            ':' => {
                let span = lx.span();
                lx.bump();
                push(Token::Colon, span);
            }
            '+' => {
                let span = lx.span();
                lx.bump();
                push(Token::Plus, span);
            }
            '-' => {
                let span = lx.span();
                lx.bump();
                push(Token::Minus, span);
            }
            '*' => {
                let span = lx.span();
                lx.bump();
                push(Token::Star, span);
            }
            '/' => {
                let span = lx.span();
                lx.bump();
                push(Token::Slash, span);
            }
            '=' => {
                let span = lx.span();
                lx.bump();
                if lx.peek() == Some('=') {
                    lx.bump();
                    push(Token::EqEq, span);
                } else {
                    push(Token::Eq, span);
                }
            }
            '!' => {
                let span = lx.span();
                lx.bump();
                if lx.peek() == Some('=') {
                    lx.bump();
                    push(Token::Ne, span);
                } else {
                    push(Token::Bang, span);
                }
            }
            '&' => {
                let span = lx.span();
                lx.bump();
                if lx.peek() == Some('&') {
                    lx.bump();
                    push(Token::AndAnd, span);
                } else {
                    return Err(
                        FirelangError::lex("unexpected char: &", span.line, span.col).into(),
                    );
                }
            }
            '|' => {
                let span = lx.span();
                lx.bump();
                if lx.peek() == Some('|') {
                    lx.bump();
                    push(Token::OrOr, span);
                } else {
                    push(Token::Pipe, span);
                }
            }
            '>' => {
                let span = lx.span();
                lx.bump();
                if lx.peek() == Some('=') {
                    lx.bump();
                    push(Token::Ge, span);
                } else {
                    push(Token::Gt, span);
                }
            }
            '<' => {
                let span = lx.span();
                lx.bump();
                if lx.peek() == Some('=') {
                    lx.bump();
                    push(Token::Le, span);
                } else {
                    push(Token::Lt, span);
                }
            }
            _ if is_ident_start(c) => {
                let span = lx.span();
                let mut s = String::new();
                while let Some(ch) = lx.peek() {
                    if is_ident_continue(ch) {
                        s.push(ch);
                        lx.bump();
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
                push(t, span);
            }
            _ => {
                let span = lx.span();
                return Err(FirelangError::lex(
                    format!("unexpected char: {c}"),
                    span.line,
                    span.col,
                )
                .into());
            }
        }
    }
    let span = lx.span();
    push(Token::Eof, span);
    Ok(tokens)
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}
