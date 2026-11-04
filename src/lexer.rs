//! Lexer: source text -> tokens. f-strings are desugared here into
//! `("" + "lit" + str(expr) + ...)` so the compiler never sees them.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Int(i64),
    Float(f64),
    Str(String),
    Ident(String),

    // keywords
    Let,
    Const,
    Global,
    Def,
    Return,
    If,
    Elif,
    Else,
    While,
    For,
    In,
    Break,
    Continue,
    Pass,
    Class,
    Import,
    Assert,
    Lambda,
    Nonlocal,
    From,
    Try,
    Except,
    Finally,
    Raise,
    As,
    And,
    Or,
    Not,
    True,
    False,
    None,
    Is,
    Del,
    With,
    Yield,

    // punctuation
    At,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Dot,
    Semi,
    Colon,

    // operators
    Plus,
    Minus,
    Star,
    StarStar,
    Slash,
    SlashSlash,
    Percent,
    Assign,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Shl,
    Shr,
    AmpEq,
    PipeEq,
    CaretEq,

    Eof,
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Tok::Int(i) => write!(f, "{}", i),
            Tok::Float(x) => write!(f, "{}", x),
            Tok::Str(s) => write!(f, "\"{}\"", s),
            Tok::Ident(s) => write!(f, "{}", s),
            other => write!(f, "{}", symbol_text(other)),
        }
    }
}

fn symbol_text(t: &Tok) -> &'static str {
    match t {
        Tok::Let => "let",
        Tok::Const => "const",
        Tok::Global => "global",
        Tok::Def => "def",
        Tok::Return => "return",
        Tok::If => "if",
        Tok::Elif => "elif",
        Tok::Else => "else",
        Tok::While => "while",
        Tok::For => "for",
        Tok::In => "in",
        Tok::Break => "break",
        Tok::Continue => "continue",
        Tok::Pass => "pass",
        Tok::Class => "class",
        Tok::Import => "import",
        Tok::Lambda => "lambda",
        Tok::Nonlocal => "nonlocal",
        Tok::From => "from",
        Tok::Try => "try",
        Tok::Except => "except",
        Tok::Finally => "finally",
        Tok::Raise => "raise",
        Tok::As => "as",
        Tok::Assert => "assert",
        Tok::And => "and",
        Tok::Or => "or",
        Tok::Not => "not",
        Tok::True => "True",
        Tok::False => "False",
        Tok::None => "None",
        Tok::Is => "is",
        Tok::Del => "del",
        Tok::With => "with",
        Tok::Yield => "yield",
        Tok::At => "@",
        Tok::LParen => "(",
        Tok::RParen => ")",
        Tok::LBrace => "{",
        Tok::RBrace => "}",
        Tok::LBracket => "[",
        Tok::RBracket => "]",
        Tok::Comma => ",",
        Tok::Dot => ".",
        Tok::Semi => ";",
        Tok::Colon => ":",
        Tok::Plus => "+",
        Tok::Minus => "-",
        Tok::Star => "*",
        Tok::StarStar => "**",
        Tok::Slash => "/",
        Tok::SlashSlash => "//",
        Tok::Percent => "%",
        Tok::Assign => "=",
        Tok::PlusEq => "+=",
        Tok::MinusEq => "-=",
        Tok::StarEq => "*=",
        Tok::SlashEq => "/=",
        Tok::PercentEq => "%=",
        Tok::EqEq => "==",
        Tok::NotEq => "!=",
        Tok::Lt => "<",
        Tok::Le => "<=",
        Tok::Gt => ">",
        Tok::Ge => ">=",
        Tok::Amp => "&",
        Tok::Pipe => "|",
        Tok::Caret => "^",
        Tok::Tilde => "~",
        Tok::Shl => "<<",
        Tok::Shr => ">>",
        Tok::AmpEq => "&=",
        Tok::PipeEq => "|=",
        Tok::CaretEq => "^=",
        Tok::Eof => "end of file",
        Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::Ident(_) => "",
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
}

pub fn lex(src: &str) -> Result<Vec<Token>, String> {
    let mut lx = Lexer {
        chars: src.chars().collect(),
        i: 0,
        line: 1,
        out: Vec::new(),
    };
    lx.run()?;
    let line = lx.line;
    lx.out.push(Token { tok: Tok::Eof, line });
    Ok(lx.out)
}

struct Lexer {
    chars: Vec<char>,
    i: usize,
    line: u32,
    out: Vec<Token>,
}

impl Lexer {
    fn peek(&self, offset: usize) -> char {
        *self.chars.get(self.i + offset).unwrap_or(&'\0')
    }

    fn at_end(&self) -> bool {
        self.i >= self.chars.len()
    }

    fn push(&mut self, tok: Tok) {
        self.out.push(Token { tok, line: self.line });
    }

    fn err(&self, msg: &str) -> String {
        format!("[line {}] SyntaxError: {}", self.line, msg)
    }

    fn run(&mut self) -> Result<(), String> {
        while !self.at_end() {
            let c = self.peek(0);
            if c == '\n' {
                self.line += 1;
                self.i += 1;
            } else if c.is_whitespace() {
                self.i += 1;
            } else if c == '#' {
                while !self.at_end() && self.peek(0) != '\n' {
                    self.i += 1;
                }
            } else if c.is_ascii_digit() {
                self.number()?;
            } else if (c == 'f' || c == 'F') && (self.peek(1) == '"' || self.peek(1) == '\'') {
                self.i += 1;
                self.fstring()?;
            } else if c.is_alphabetic() || c == '_' {
                self.ident();
            } else if c == '"' || c == '\'' {
                let line = self.line;
                let s = self.string()?;
                self.out.push(Token { tok: Tok::Str(s), line });
            } else {
                self.symbol()?;
            }
        }
        Ok(())
    }

    fn number(&mut self) -> Result<(), String> {
        if self.peek(0) == '0' && (self.peek(1) == 'x' || self.peek(1) == 'X') {
            self.i += 2;
            let start = self.i;
            while self.peek(0).is_ascii_hexdigit() || self.peek(0) == '_' {
                self.i += 1;
            }
            let text: String = self.chars[start..self.i].iter().filter(|c| **c != '_').collect();
            let v = i64::from_str_radix(&text, 16).map_err(|_| self.err("invalid hex literal"))?;
            self.push(Tok::Int(v));
            return Ok(());
        }

        let start = self.i;
        let mut is_float = false;
        while self.peek(0).is_ascii_digit() || self.peek(0) == '_' {
            self.i += 1;
        }
        if self.peek(0) == '.' && self.peek(1).is_ascii_digit() {
            is_float = true;
            self.i += 1;
            while self.peek(0).is_ascii_digit() || self.peek(0) == '_' {
                self.i += 1;
            }
        }
        if self.peek(0) == 'e' || self.peek(0) == 'E' {
            let mut j = 1;
            if self.peek(1) == '+' || self.peek(1) == '-' {
                j = 2;
            }
            if self.peek(j).is_ascii_digit() {
                is_float = true;
                self.i += j;
                while self.peek(0).is_ascii_digit() {
                    self.i += 1;
                }
            }
        }
        let text: String = self.chars[start..self.i].iter().filter(|c| **c != '_').collect();
        let tok = if is_float {
            Tok::Float(text.parse().map_err(|_| self.err(&format!("invalid number '{}'", text)))?)
        } else {
            Tok::Int(
                text.parse()
                    .map_err(|_| self.err(&format!("integer literal too large: {}", text)))?,
            )
        };
        self.push(tok);
        Ok(())
    }

    fn ident(&mut self) {
        let start = self.i;
        while self.peek(0).is_alphanumeric() || self.peek(0) == '_' {
            self.i += 1;
        }
        let word: String = self.chars[start..self.i].iter().collect();
        let tok = match word.as_str() {
            "let" => Tok::Let,
            "const" => Tok::Const,
            "global" => Tok::Global,
            "def" => Tok::Def,
            "return" => Tok::Return,
            "if" => Tok::If,
            "elif" => Tok::Elif,
            "else" => Tok::Else,
            "while" => Tok::While,
            "for" => Tok::For,
            "in" => Tok::In,
            "break" => Tok::Break,
            "continue" => Tok::Continue,
            "pass" => Tok::Pass,
            "class" => Tok::Class,
            "import" => Tok::Import,
            "lambda" => Tok::Lambda,
            "nonlocal" => Tok::Nonlocal,
            "from" => Tok::From,
            "try" => Tok::Try,
            "except" => Tok::Except,
            "finally" => Tok::Finally,
            "raise" => Tok::Raise,
            "as" => Tok::As,
            "assert" => Tok::Assert,
            "and" => Tok::And,
            "or" => Tok::Or,
            "not" => Tok::Not,
            "True" => Tok::True,
            "False" => Tok::False,
            "None" => Tok::None,
            "is" => Tok::Is,
            "del" => Tok::Del,
            "with" => Tok::With,
            "yield" => Tok::Yield,
            _ => Tok::Ident(word),
        };
        self.push(tok);
    }

    /// Reads a quoted string starting at the opening quote; processes escapes.
    fn string(&mut self) -> Result<String, String> {
        let quote = self.peek(0);
        self.i += 1;
        let mut s = String::new();
        loop {
            if self.at_end() {
                return Err(self.err("unterminated string"));
            }
            let c = self.peek(0);
            self.i += 1;
            if c == quote {
                break;
            }
            if c == '\n' {
                self.line += 1;
            }
            if c == '\\' {
                if self.at_end() {
                    return Err(self.err("unterminated string"));
                }
                let e = self.peek(0);
                self.i += 1;
                match e {
                    'n' => s.push('\n'),
                    't' => s.push('\t'),
                    'r' => s.push('\r'),
                    '0' => s.push('\0'),
                    '\\' => s.push('\\'),
                    '\'' => s.push('\''),
                    '"' => s.push('"'),
                    '\n' => self.line += 1, // line continuation
                    other => {
                        s.push('\\');
                        s.push(other);
                    }
                }
            } else {
                s.push(c);
            }
        }
        Ok(s)
    }

    /// f"{name} hp: {hp:.1f}!"  ->  $fmt((name), " hp: ", $format((hp), ".1f"), "!")
    /// (`$`-names can't be written in source, so user code can't shadow them).
    fn fstring(&mut self) -> Result<(), String> {
        let line = self.line;
        let content = self.string()?;
        let cs: Vec<char> = content.chars().collect();
        let mut toks = vec![Tok::Ident("$fmt".into()), Tok::LParen];
        let mut lit = String::new();
        let mut first = true;
        let mut sep = |toks: &mut Vec<Tok>| {
            if !first {
                toks.push(Tok::Comma);
            }
            first = false;
        };
        let mut j = 0;
        while j < cs.len() {
            let c = cs[j];
            if c == '{' && cs.get(j + 1) == Some(&'{') {
                lit.push('{');
                j += 2;
                continue;
            }
            if c == '}' && cs.get(j + 1) == Some(&'}') {
                lit.push('}');
                j += 2;
                continue;
            }
            if c == '}' {
                return Err(format!("[line {}] SyntaxError: single '}}' in f-string", line));
            }
            if c == '{' {
                if !lit.is_empty() {
                    sep(&mut toks);
                    toks.push(Tok::Str(std::mem::take(&mut lit)));
                }
                // Find the matching '}' and an optional top-level ':' format spec.
                let mut depth = 0;
                let mut k = j + 1;
                let mut quote: Option<char> = None;
                let mut colon: Option<usize> = None;
                while k < cs.len() {
                    let ch = cs[k];
                    if let Some(q) = quote {
                        if ch == q {
                            quote = None;
                        }
                    } else if ch == '"' || ch == '\'' {
                        quote = Some(ch);
                    } else if ch == '{' || ch == '[' || ch == '(' {
                        depth += 1;
                    } else if ch == ')' || ch == ']' {
                        depth -= 1;
                    } else if ch == '}' {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    } else if ch == ':' && depth == 0 && colon.is_none() {
                        colon = Some(k);
                    }
                    k += 1;
                }
                if k >= cs.len() {
                    return Err(format!("[line {}] SyntaxError: unterminated '{{' in f-string", line));
                }
                let expr_end = colon.unwrap_or(k);
                let inner: String = cs[j + 1..expr_end].iter().collect();
                if inner.trim().is_empty() {
                    return Err(format!("[line {}] SyntaxError: empty expression in f-string", line));
                }
                let mut inner_toks = lex(&inner)
                    .map_err(|e| format!("[line {}] SyntaxError in f-string: {}", line, e))?;
                inner_toks.pop(); // Eof
                sep(&mut toks);
                match colon {
                    // `$fmt` str()-converts plain pieces itself.
                    None => {
                        toks.push(Tok::LParen);
                        toks.extend(inner_toks.into_iter().map(|t| t.tok));
                        toks.push(Tok::RParen);
                    }
                    Some(cpos) => {
                        let spec: String = cs[cpos + 1..k].iter().collect();
                        toks.push(Tok::Ident("$format".into()));
                        toks.push(Tok::LParen);
                        toks.push(Tok::LParen);
                        toks.extend(inner_toks.into_iter().map(|t| t.tok));
                        toks.push(Tok::RParen);
                        toks.push(Tok::Comma);
                        toks.push(Tok::Str(spec));
                        toks.push(Tok::RParen);
                    }
                }
                j = k + 1;
                continue;
            }
            lit.push(c);
            j += 1;
        }
        if !lit.is_empty() {
            sep(&mut toks);
            toks.push(Tok::Str(lit));
        }
        toks.push(Tok::RParen);
        for tok in toks {
            self.out.push(Token { tok, line });
        }
        Ok(())
    }

    fn symbol(&mut self) -> Result<(), String> {
        let c = self.peek(0);
        let n = self.peek(1);
        let (tok, len) = match (c, n) {
            ('*', '*') => (Tok::StarStar, 2),
            ('/', '/') => (Tok::SlashSlash, 2),
            ('=', '=') => (Tok::EqEq, 2),
            ('!', '=') => (Tok::NotEq, 2),
            ('<', '=') => (Tok::Le, 2),
            ('>', '=') => (Tok::Ge, 2),
            ('+', '=') => (Tok::PlusEq, 2),
            ('-', '=') => (Tok::MinusEq, 2),
            ('*', '=') => (Tok::StarEq, 2),
            ('/', '=') => (Tok::SlashEq, 2),
            ('%', '=') => (Tok::PercentEq, 2),
            ('&', '&') => (Tok::And, 2),
            ('|', '|') => (Tok::Or, 2),
            ('&', '=') => (Tok::AmpEq, 2),
            ('|', '=') => (Tok::PipeEq, 2),
            ('^', '=') => (Tok::CaretEq, 2),
            ('<', '<') => (Tok::Shl, 2),
            ('>', '>') => (Tok::Shr, 2),
            ('&', _) => (Tok::Amp, 1),
            ('|', _) => (Tok::Pipe, 1),
            ('^', _) => (Tok::Caret, 1),
            ('~', _) => (Tok::Tilde, 1),
            ('@', _) => (Tok::At, 1),
            ('!', _) => (Tok::Not, 1),
            ('(', _) => (Tok::LParen, 1),
            (')', _) => (Tok::RParen, 1),
            ('{', _) => (Tok::LBrace, 1),
            ('}', _) => (Tok::RBrace, 1),
            ('[', _) => (Tok::LBracket, 1),
            (']', _) => (Tok::RBracket, 1),
            (',', _) => (Tok::Comma, 1),
            ('.', _) => (Tok::Dot, 1),
            (';', _) => (Tok::Semi, 1),
            (':', _) => (Tok::Colon, 1),
            ('+', _) => (Tok::Plus, 1),
            ('-', _) => (Tok::Minus, 1),
            ('*', _) => (Tok::Star, 1),
            ('/', _) => (Tok::Slash, 1),
            ('%', _) => (Tok::Percent, 1),
            ('=', _) => (Tok::Assign, 1),
            ('<', _) => (Tok::Lt, 1),
            ('>', _) => (Tok::Gt, 1),
            _ => return Err(self.err(&format!("unexpected character '{}'", c))),
        };
        self.i += len;
        self.push(tok);
        Ok(())
    }
}
