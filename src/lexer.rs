use std::error::Error;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Int(i64),
    Str(String),
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
    LParen,
    RParen,
    Comma,
    Dot,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Newline,
    Eof,
}

pub fn lex(input: &str) -> Result<Vec<Token>, Box<dyn Error>> {
    let mut chars = input.chars().peekable();
    let mut tokens = Vec::new();

    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\r' => {
                chars.next();
            }
            '\n' | ';' => {
                chars.next();
                tokens.push(Token::Newline);
            }
            '#' => {
                while let Some(&cc) = chars.peek() {
                    chars.next();
                    if cc == '\n' {
                        tokens.push(Token::Newline);
                        break;
                    }
                }
            }
            '0'..='9' => {
                let mut s = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() {
                        s.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Int(s.parse()?));
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    let ch = match chars.next() {
                        None => return Err("unterminated string literal".into()),
                        Some(c) => c,
                    };
                    match ch {
                        '"' => break,
                        '\\' => {
                            let esc = match chars.next() {
                                None => return Err("unterminated string literal".into()),
                                Some(e) => e,
                            };
                            s.push(match esc {
                                'n' => '\n',
                                'r' => '\r',
                                't' => '\t',
                                '"' => '"',
                                '\\' => '\\',
                                other => other,
                            });
                        }
                        other => s.push(other),
                    }
                }
                tokens.push(Token::Str(s));
            }
            '(' => {
                chars.next();
                tokens.push(Token::LParen);
            }
            ')' => {
                chars.next();
                tokens.push(Token::RParen);
            }
            ',' => {
                chars.next();
                tokens.push(Token::Comma);
            }
            '{' => {
                chars.next();
                tokens.push(Token::LBrace);
            }
            '}' => {
                chars.next();
                tokens.push(Token::RBrace);
            }
            '[' => {
                chars.next();
                tokens.push(Token::LBracket);
            }
            ']' => {
                chars.next();
                tokens.push(Token::RBracket);
            }
            '.' => {
                chars.next();
                tokens.push(Token::Dot);
            }
            ':' => {
                chars.next();
                tokens.push(Token::Colon);
            }
            '+' => {
                chars.next();
                tokens.push(Token::Plus);
            }
            '-' => {
                chars.next();
                tokens.push(Token::Minus);
            }
            '*' => {
                chars.next();
                tokens.push(Token::Star);
            }
            '/' => {
                chars.next();
                tokens.push(Token::Slash);
            }
            '=' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    tokens.push(Token::EqEq);
                } else {
                    tokens.push(Token::Eq);
                }
            }
            '!' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    tokens.push(Token::Ne);
                } else {
                    return Err("unexpected !".into());
                }
            }
            '>' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    tokens.push(Token::Ge);
                } else {
                    tokens.push(Token::Gt);
                }
            }
            '<' => {
                chars.next();
                if chars.peek() == Some(&'=') {
                    chars.next();
                    tokens.push(Token::Le);
                } else {
                    tokens.push(Token::Lt);
                }
            }
            _ if is_ident_start(c) => {
                let mut s = String::new();
                while let Some(&ch) = chars.peek() {
                    if is_ident_continue(ch) {
                        s.push(ch);
                        chars.next();
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
                tokens.push(t);
            }
            _ => return Err(format!("unexpected char: {c}").into()),
        }
    }
    tokens.push(Token::Eof);
    Ok(tokens)
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}
