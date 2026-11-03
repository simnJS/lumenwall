//! Preprocessing-token lexer (C rules, GLSL punctuators).

use super::types::{Interner, Name};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum P {
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Dot,
    Comma,
    Colon,
    Semicolon,
    Question,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Lt,
    Gt,
    Le,
    Ge,
    EqEq,
    Ne,
    AndAnd,
    OrOr,
    XorXor,
    Bang,
    Tilde,
    Amp,
    Pipe,
    Caret,
    Shl,
    Shr,
    Assign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    ModAssign,
    ShlAssign,
    ShrAssign,
    AndAssign,
    OrAssign,
    XorAssign,
    Inc,
    Dec,
    Hash,
    HashHash,
}

impl P {
    pub fn text(self) -> &'static str {
        use P::*;
        match self {
            LParen => "(",
            RParen => ")",
            LBracket => "[",
            RBracket => "]",
            LBrace => "{",
            RBrace => "}",
            Dot => ".",
            Comma => ",",
            Colon => ":",
            Semicolon => ";",
            Question => "?",
            Plus => "+",
            Minus => "-",
            Star => "*",
            Slash => "/",
            Percent => "%",
            Lt => "<",
            Gt => ">",
            Le => "<=",
            Ge => ">=",
            EqEq => "==",
            Ne => "!=",
            AndAnd => "&&",
            OrOr => "||",
            XorXor => "^^",
            Bang => "!",
            Tilde => "~",
            Amp => "&",
            Pipe => "|",
            Caret => "^",
            Shl => "<<",
            Shr => ">>",
            Assign => "=",
            AddAssign => "+=",
            SubAssign => "-=",
            MulAssign => "*=",
            DivAssign => "/=",
            ModAssign => "%=",
            ShlAssign => "<<=",
            ShrAssign => ">>=",
            AndAssign => "&=",
            OrAssign => "|=",
            XorAssign => "^=",
            Inc => "++",
            Dec => "--",
            Hash => "#",
            HashHash => "##",
        }
    }
}

/// Punctuators sorted so that longer spellings are tried first.
const PUNCTS: &[(&str, P)] = &[
    ("<<=", P::ShlAssign),
    (">>=", P::ShrAssign),
    ("<=", P::Le),
    (">=", P::Ge),
    ("==", P::EqEq),
    ("!=", P::Ne),
    ("&&", P::AndAnd),
    ("||", P::OrOr),
    ("^^", P::XorXor),
    ("<<", P::Shl),
    (">>", P::Shr),
    ("+=", P::AddAssign),
    ("-=", P::SubAssign),
    ("*=", P::MulAssign),
    ("/=", P::DivAssign),
    ("%=", P::ModAssign),
    ("&=", P::AndAssign),
    ("|=", P::OrAssign),
    ("^=", P::XorAssign),
    ("++", P::Inc),
    ("--", P::Dec),
    ("##", P::HashHash),
    ("(", P::LParen),
    (")", P::RParen),
    ("[", P::LBracket),
    ("]", P::RBracket),
    ("{", P::LBrace),
    ("}", P::RBrace),
    (".", P::Dot),
    (",", P::Comma),
    (":", P::Colon),
    (";", P::Semicolon),
    ("?", P::Question),
    ("+", P::Plus),
    ("-", P::Minus),
    ("*", P::Star),
    ("/", P::Slash),
    ("%", P::Percent),
    ("<", P::Lt),
    (">", P::Gt),
    ("!", P::Bang),
    ("~", P::Tilde),
    ("&", P::Amp),
    ("|", P::Pipe),
    ("^", P::Caret),
    ("=", P::Assign),
    ("#", P::Hash),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokKind {
    Ident(Name),
    /// Preprocessing number; the payload is its interned spelling.
    Number(Name),
    Punct(P),
    Other(char),
    Newline,
}

#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub kind: TokKind,
    pub line: u32,
    /// Whitespace precedes the token.
    pub space: bool,
    /// Hide set (macro expansion), index into the preprocessor's table.
    pub hs: u32,
}

impl Token {
    pub fn is_punct(&self, p: P) -> bool {
        self.kind == TokKind::Punct(p)
    }
}

/// Spelling of a token.
pub fn spelling(t: &Token, int: &Interner) -> String {
    match t.kind {
        TokKind::Ident(n) | TokKind::Number(n) => int.get(n).to_string(),
        TokKind::Punct(p) => p.text().to_string(),
        TokKind::Other(c) => c.to_string(),
        TokKind::Newline => "\n".to_string(),
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}
fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Lexes `src` into preprocessing tokens, with explicit `Newline` tokens.
/// Comments become whitespace; backslash-newline splices lines.
pub fn lex(src: &str, int: &mut Interner, first_line: u32, out: &mut Vec<Token>) {
    let b = src.as_bytes();
    let mut i = 0;
    let mut line = first_line;
    let mut space = false;
    while i < b.len() {
        let c = b[i];
        // Line splice.
        if c == b'\\' {
            if b.get(i + 1) == Some(&b'\n') {
                i += 2;
                line += 1;
                continue;
            }
            if b.get(i + 1) == Some(&b'\r') && b.get(i + 2) == Some(&b'\n') {
                i += 3;
                line += 1;
                continue;
            }
        }
        match c {
            b'\n' => {
                out.push(Token {
                    kind: TokKind::Newline,
                    line,
                    space,
                    hs: 0,
                });
                line += 1;
                space = false;
                i += 1;
            }
            b' ' | b'\t' | b'\r' | 0x0b | 0x0c => {
                space = true;
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                // Line comment, honouring splices.
                i += 2;
                while i < b.len() && b[i] != b'\n' {
                    if b[i] == b'\\' && b.get(i + 1) == Some(&b'\n') {
                        line += 1;
                        i += 2;
                        continue;
                    }
                    i += 1;
                }
                space = true;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    if b[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i = (i + 2).min(b.len());
                space = true;
            }
            _ if is_ident_start(c) => {
                let s = i;
                while i < b.len() && is_ident_cont(b[i]) {
                    i += 1;
                }
                let n = int.intern(&src[s..i]);
                out.push(Token {
                    kind: TokKind::Ident(n),
                    line,
                    space,
                    hs: 0,
                });
                space = false;
            }
            _ if c.is_ascii_digit()
                || (c == b'.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) =>
            {
                let s = i;
                i += 1;
                while i < b.len() {
                    let d = b[i];
                    if (d == b'+' || d == b'-') && matches!(b[i - 1], b'e' | b'E') {
                        // Exponent sign, except in hex numbers ("0x1e+1" is three tokens).
                        let is_hex = src[s..].starts_with("0x") || src[s..].starts_with("0X");
                        if is_hex {
                            break;
                        }
                        i += 1;
                    } else if is_ident_cont(d) || d == b'.' {
                        i += 1;
                    } else {
                        break;
                    }
                }
                let n = int.intern(&src[s..i]);
                out.push(Token {
                    kind: TokKind::Number(n),
                    line,
                    space,
                    hs: 0,
                });
                space = false;
            }
            _ if c.is_ascii() => {
                let rest = &src[i..];
                if let Some(&(txt, p)) = PUNCTS.iter().find(|(t, _)| rest.starts_with(t)) {
                    out.push(Token {
                        kind: TokKind::Punct(p),
                        line,
                        space,
                        hs: 0,
                    });
                    i += txt.len();
                } else {
                    out.push(Token {
                        kind: TokKind::Other(c as char),
                        line,
                        space,
                        hs: 0,
                    });
                    i += 1;
                }
                space = false;
            }
            _ => {
                let ch = src[i..].chars().next().unwrap_or('?');
                out.push(Token {
                    kind: TokKind::Other(ch),
                    line,
                    space,
                    hs: 0,
                });
                i += ch.len_utf8();
                space = false;
            }
        }
    }
}
