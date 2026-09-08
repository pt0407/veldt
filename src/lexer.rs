// Lexer for the Veldt language

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Literals
    Int(i64),
    Float(f64),
    Str(String),
    InterpStr(Vec<String>), // alternating literal, expr-source, literal, ...
    Ident(String),
    // Keywords
    Let, Fn, If, Else, While, For, Return, Print, Grow, Struct, Test, Try, Catch, Import,
    True, False, And, Or, Not,
    // Type names
    TypeInt, TypeStr, TypeBool, TypeList, TypeFloat, TypeFn,
    // Operators
    Plus, Minus, Star, Slash, Percent,
    Eq, EqEq, Neq, Lt, Gt, Le, Ge,
    // Delimiters
    LParen, RParen, LBrace, RBrace, LBracket, RBracket,
    Comma, Colon, Semicolon, Dot,
    // Variant marker: sort#2
    Hash(u32),
    // End of file
    Eof,
}

pub struct Lexer {
    src: Vec<char>,
    pos: usize,
    line: usize,
    pub token_lines: Vec<usize>,
    // Track if last token was an identifier AND no whitespace followed it
    last_was_ident_no_space: bool,
}

impl Lexer {
    pub fn new(src: &str) -> Self {
        Lexer {
            src: src.chars().collect(),
            pos: 0,
            line: 1,
            token_lines: Vec::new(),
            last_was_ident_no_space: false,
        }
    }

    fn peek(&self) -> Option<char> {
        self.src.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.src.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek();
        if let Some(ch) = c {
            self.pos += 1;
            if ch == '\n' {
                self.line += 1;
            }
        }
        c
    }

    fn skip_whitespace(&mut self) {
        let mut had_ws = false;
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.advance();
                had_ws = true;
            } else {
                break;
            }
        }
        if had_ws {
            self.last_was_ident_no_space = false;
        }
    }

    fn read_string(&mut self) -> Result<Token, String> {
        self.advance(); // skip opening "
        let mut s = String::new();
        let mut parts: Vec<String> = Vec::new();
        let mut has_interp = false;
        loop {
            match self.advance() {
                None => return Err("Unterminated string".into()),
                Some('"') => break,
                Some('\\') => {
                    match self.advance() {
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some('"') => s.push('"'),
                        Some('\\') => s.push('\\'),
                        Some('{') => s.push('{'),
                        Some('}') => s.push('}'),
                        Some(c) => s.push(c),
                        None => return Err("Unterminated string".into()),
                    }
                }
                Some('{') => {
                    // Check for {{ (escaped literal {)
                    if self.peek() == Some('{') {
                        self.advance();
                        s.push('{');
                        continue;
                    }
                    // Start interpolation
                    has_interp = true;
                    parts.push(std::mem::take(&mut s));
                    let mut expr_src = String::new();
                    let mut depth = 1;
                    loop {
                        match self.advance() {
                            None => return Err("Unterminated interpolation in string".into()),
                            Some('}') => {
                                depth -= 1;
                                if depth == 0 { break; }
                                expr_src.push('}');
                            }
                            Some('{') => {
                                depth += 1;
                                expr_src.push('{');
                            }
                            Some(c) => expr_src.push(c),
                        }
                    }
                    parts.push(expr_src);
                }
                Some('}') => {
                    // Check for }} (escaped literal })
                    if self.peek() == Some('}') {
                        self.advance();
                        s.push('}');
                    } else {
                        s.push('}');
                    }
                }
                Some(c) => s.push(c),
            }
        }
        if has_interp {
            parts.push(s); // final literal part
            Ok(Token::InterpStr(parts))
        } else {
            Ok(Token::Str(s))
        }
    }

    fn read_number(&mut self) -> Result<Token, String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.advance();
            } else {
                break;
            }
        }
        // Check for decimal point
        let mut is_float = false;
        if self.peek() == Some('.') {
            // Make sure next char after . is a digit (not a method call like x.field)
            if let Some(c2) = self.peek_at(1) {
                if c2.is_ascii_digit() {
                    is_float = true;
                    self.advance(); // skip .
                    while let Some(c) = self.peek() {
                        if c.is_ascii_digit() {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
            }
        }
        let s: String = self.src[start..self.pos].iter().collect();
        if is_float {
            s.parse::<f64>()
                .map(Token::Float)
                .map_err(|e| e.to_string())
        } else {
            s.parse::<i64>()
                .map(Token::Int)
                .map_err(|e| e.to_string())
        }
    }

    fn read_ident(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' {
                self.advance();
            } else {
                break;
            }
        }
        self.src[start..self.pos].iter().collect()
    }

    fn keyword_or_ident(&self, s: &str) -> Token {
        match s {
            "let" => Token::Let,
            "fn" => Token::Fn,
            "if" => Token::If,
            "else" => Token::Else,
            "while" => Token::While,
            "for" => Token::For,
            "return" => Token::Return,
            "print" => Token::Print,
            "grow" => Token::Grow,
            "struct" => Token::Struct,
            "test" => Token::Test,
            "try" => Token::Try,
            "catch" => Token::Catch,
            "import" => Token::Import,
            "true" => Token::True,
            "false" => Token::False,
            "and" => Token::And,
            "or" => Token::Or,
            "not" => Token::Not,
            "int" => Token::TypeInt,
            "str" => Token::TypeStr,
            "bool" => Token::TypeBool,
            "list" => Token::TypeList,
            "float" => Token::TypeFloat,
            _ => Token::Ident(s.to_string()),
        }
    }

    pub fn tokenize(&mut self) -> Result<Vec<Token>, String> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace();
            let tok_line = self.line;
            match self.peek() {
                None => {
                    self.token_lines.push(tok_line); tokens.push(Token::Eof);
                    break;
                }
                Some('#') => {
                    // # after identifier = variant marker, otherwise comment
                    if self.last_was_ident_no_space {
                        self.advance(); // skip #
                        // read the number after #
                        let n = self.read_number()?;
                        let n_val = match n {
                            Token::Int(v) => v as u32,
                            _ => 0,
                        };
                        self.token_lines.push(tok_line); tokens.push(Token::Hash(n_val));
                        self.last_was_ident_no_space = false;
                    } else {
                        // comment: skip to end of line
                        while let Some(c) = self.peek() {
                            if c == '\n' { break; }
                            self.advance();
                        }
                    }
                }
                Some('"') => {
                    let tok = self.read_string()?;
                    self.token_lines.push(tok_line); tokens.push(tok);
                    self.last_was_ident_no_space = false;
                }
                Some(c) if c.is_ascii_digit() => {
                    let tok = self.read_number()?;
                    self.token_lines.push(tok_line); tokens.push(tok);
                    self.last_was_ident_no_space = false;
                }
                Some(c) if c.is_alphabetic() || c == '_' => {
                    let s = self.read_ident();
                    let tok = self.keyword_or_ident(&s);
                    let is_ident = matches!(tok, Token::Ident(_));
                    self.token_lines.push(tok_line); tokens.push(tok);
                    self.last_was_ident_no_space = is_ident;
                }
                Some('+') => { self.advance(); self.token_lines.push(tok_line); tokens.push(Token::Plus); self.last_was_ident_no_space = false; }
                Some('-') => { self.advance(); self.token_lines.push(tok_line); tokens.push(Token::Minus); self.last_was_ident_no_space = false; }
                Some('*') => { self.advance(); self.token_lines.push(tok_line); tokens.push(Token::Star); self.last_was_ident_no_space = false; }
                Some('/') => { self.advance(); self.token_lines.push(tok_line); tokens.push(Token::Slash); self.last_was_ident_no_space = false; }
                Some('%') => { self.advance(); self.token_lines.push(tok_line); tokens.push(Token::Percent); self.last_was_ident_no_space = false; }
                Some('(') => { self.advance(); self.token_lines.push(tok_line); tokens.push(Token::LParen); self.last_was_ident_no_space = false; }
                Some(')') => { self.advance(); self.token_lines.push(tok_line); tokens.push(Token::RParen); self.last_was_ident_no_space = false; }
                Some('{') => { self.advance(); self.token_lines.push(tok_line); tokens.push(Token::LBrace); self.last_was_ident_no_space = false; }
                Some('}') => { self.advance(); self.token_lines.push(tok_line); tokens.push(Token::RBrace); self.last_was_ident_no_space = false; }
                Some('[') => { self.advance(); self.token_lines.push(tok_line); tokens.push(Token::LBracket); self.last_was_ident_no_space = false; }
                Some(']') => { self.advance(); self.token_lines.push(tok_line); tokens.push(Token::RBracket); self.last_was_ident_no_space = false; }
                Some(',') => { self.advance(); self.token_lines.push(tok_line); tokens.push(Token::Comma); self.last_was_ident_no_space = false; }
                Some(':') => { self.advance(); self.token_lines.push(tok_line); tokens.push(Token::Colon); self.last_was_ident_no_space = false; }
                Some(';') => { self.advance(); self.token_lines.push(tok_line); tokens.push(Token::Semicolon); self.last_was_ident_no_space = false; }
                Some('.') => { self.advance(); self.token_lines.push(tok_line); tokens.push(Token::Dot); self.last_was_ident_no_space = false; }
                Some('=') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        self.token_lines.push(tok_line); tokens.push(Token::EqEq);
                    } else {
                        self.token_lines.push(tok_line); tokens.push(Token::Eq);
                    }
                    self.last_was_ident_no_space = false;
                }
                Some('!') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        self.token_lines.push(tok_line); tokens.push(Token::Neq);
                        self.last_was_ident_no_space = false;
                    } else {
                        return Err("Unexpected '!'".into());
                    }
                }
                Some('<') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        self.token_lines.push(tok_line); tokens.push(Token::Le);
                    } else {
                        self.token_lines.push(tok_line); tokens.push(Token::Lt);
                    }
                    self.last_was_ident_no_space = false;
                }
                Some('>') => {
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        self.token_lines.push(tok_line); tokens.push(Token::Ge);
                    } else {
                        self.token_lines.push(tok_line); tokens.push(Token::Gt);
                    }
                    self.last_was_ident_no_space = false;
                }
                Some(c) => return Err(format!("Unexpected character: '{}'", c)),
            }
        }
        Ok(tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_tokens() {
        let mut lex = Lexer::new("let x = 5");
        let tokens = lex.tokenize().unwrap();
        assert_eq!(tokens[0], Token::Let);
        assert_eq!(tokens[1], Token::Ident("x".into()));
        assert_eq!(tokens[2], Token::Eq);
        assert_eq!(tokens[3], Token::Int(5));
    }

    #[test]
    fn test_string() {
        let mut lex = Lexer::new("\"hello world\"");
        let tokens = lex.tokenize().unwrap();
        assert_eq!(tokens[0], Token::Str("hello world".into()));
    }

    #[test]
    fn test_comment_ignored() {
        let mut lex = Lexer::new("x # this is a comment\ny");
        let tokens = lex.tokenize().unwrap();
        assert_eq!(tokens[0], Token::Ident("x".into()));
        assert_eq!(tokens[1], Token::Ident("y".into()));
    }

    #[test]
    fn test_variant_marker() {
        let mut lex = Lexer::new("sort#2");
        let tokens = lex.tokenize().unwrap();
        assert_eq!(tokens[0], Token::Ident("sort".into()));
        assert_eq!(tokens[1], Token::Hash(2));
    }

    #[test]
    fn test_operators() {
        let mut lex = Lexer::new("== != <= >= < > + - * / %");
        let tokens = lex.tokenize().unwrap();
        assert_eq!(tokens[0], Token::EqEq);
        assert_eq!(tokens[1], Token::Neq);
        assert_eq!(tokens[2], Token::Le);
        assert_eq!(tokens[3], Token::Ge);
        assert_eq!(tokens[4], Token::Lt);
        assert_eq!(tokens[5], Token::Gt);
        assert_eq!(tokens[6], Token::Plus);
        assert_eq!(tokens[7], Token::Minus);
        assert_eq!(tokens[8], Token::Star);
        assert_eq!(tokens[9], Token::Slash);
        assert_eq!(tokens[10], Token::Percent);
    }

    #[test]
    fn test_keywords() {
        let mut lex = Lexer::new("fn let if else while for return print grow struct test");
        let tokens = lex.tokenize().unwrap();
        assert_eq!(tokens[0], Token::Fn);
        assert_eq!(tokens[1], Token::Let);
        assert_eq!(tokens[2], Token::If);
        assert_eq!(tokens[3], Token::Else);
        assert_eq!(tokens[4], Token::While);
        assert_eq!(tokens[5], Token::For);
        assert_eq!(tokens[6], Token::Return);
        assert_eq!(tokens[7], Token::Print);
        assert_eq!(tokens[8], Token::Grow);
        assert_eq!(tokens[9], Token::Struct);
        assert_eq!(tokens[10], Token::Test);
    }
}
