use crate::reporter::{BytePos, FileId, LspPos, Span};

use super::error::LexError;
use super::token::{keyword_lookup, Token, TokenKind};

pub(super) struct Lexer<'a> {
    src: &'a str,
    file: FileId,
    pos: usize,
    line: u32,
    col: u32,
}

type Mark = (BytePos, LspPos);

impl<'a> Lexer<'a> {
    pub(super) fn new(src: &'a str, file: FileId) -> Self {
        Self { src, file, pos: 0, line: 0, col: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek2(&self) -> Option<char> {
        self.src[self.pos..].chars().nth(1)
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.src[self.pos..].chars().nth(n)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        if c == '\n' {
            self.line += 1;
            self.col = 0;
        } else {
            self.col += c.len_utf16() as u32;
        }
        Some(c)
    }

    fn mark(&self) -> Mark {
        (BytePos::new(self.pos), LspPos::new(self.line, self.col))
    }

    fn span_from(&self, start: Mark) -> Span {
        Span {
            file: self.file,
            start: start.0,
            end: BytePos::new(self.pos),
            lsp_start: start.1,
            lsp_end: LspPos::new(self.line, self.col),
        }
    }

    /// Skip whitespace and comments. Returns `Some(error_token)` if an
    /// unterminated block comment was found (we'll be at EOF afterwards).
    fn skip_trivia(&mut self) -> Option<Token> {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\r' | '\n') => {
                    self.bump();
                }
                Some('/') if self.peek2() == Some('/') => {
                    self.bump();
                    self.bump();
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('/') if self.peek2() == Some('*') => {
                    let start = self.mark();
                    self.bump();
                    self.bump();
                    let mut depth: u32 = 1;
                    while depth > 0 {
                        match (self.peek(), self.peek2()) {
                            (None, _) => {
                                return Some(Token {
                                    kind: TokenKind::Error(LexError::UnterminatedBlockComment),
                                    span: self.span_from(start),
                                });
                            }
                            (Some('/'), Some('*')) => {
                                self.bump();
                                self.bump();
                                depth += 1;
                            }
                            (Some('*'), Some('/')) => {
                                self.bump();
                                self.bump();
                                depth -= 1;
                            }
                            _ => {
                                self.bump();
                            }
                        }
                    }
                }
                _ => return None,
            }
        }
    }

    pub(super) fn lex(mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        loop {
            if let Some(err) = self.skip_trivia() {
                tokens.push(err);
                let start = self.mark();
                tokens.push(Token { kind: TokenKind::Eof, span: self.span_from(start) });
                return tokens;
            }
            let start = self.mark();
            let Some(c) = self.peek() else {
                tokens.push(Token { kind: TokenKind::Eof, span: self.span_from(start) });
                return tokens;
            };
            if c == '_' || c.is_ascii_alphabetic() {
                tokens.push(self.scan_ident(start));
            } else if c.is_ascii_digit() {
                tokens.push(self.scan_number(start));
            } else if c == '\'' {
                tokens.push(self.scan_char(start));
            } else if c == '"' {
                self.scan_string(start, &mut tokens);
            } else {
                tokens.push(self.scan_op_or_unexpected(start, c));
            }
        }
    }

    fn scan_ident(&mut self, start: Mark) -> Token {
        let begin = self.pos;
        while let Some(c) = self.peek() {
            if c == '_' || c.is_ascii_alphanumeric() {
                self.bump();
            } else {
                break;
            }
        }
        let s = &self.src[begin..self.pos];
        let kind = keyword_lookup(s).unwrap_or_else(|| TokenKind::Ident(s.to_string()));
        Token { kind, span: self.span_from(start) }
    }

    fn scan_number(&mut self, start: Mark) -> Token {
        let first = self.bump().expect("scan_number called at EOF");
        let radix: u32;
        let mut had_prefix = false;
        if first == '0' {
            match self.peek() {
                Some('x' | 'X') => {
                    self.bump();
                    radix = 16;
                    had_prefix = true;
                }
                Some('b' | 'B') => {
                    self.bump();
                    radix = 2;
                    had_prefix = true;
                }
                _ => radix = 10,
            }
        } else {
            radix = 10;
        }

        let mut digits = String::new();
        if !had_prefix {
            digits.push(first);
        }
        let mut invalid = false;
        while let Some(c) = self.peek() {
            if c == '_' {
                self.bump();
                continue;
            }
            if c.is_digit(radix) {
                digits.push(c);
                self.bump();
            } else if c.is_ascii_digit() {
                // A digit, but not valid for this base (e.g. '8' in 0b...).
                digits.push(c);
                self.bump();
                invalid = true;
            } else {
                // Letter or other — terminate the literal.
                break;
            }
        }

        let span = self.span_from(start);
        if invalid {
            return Token { kind: TokenKind::Error(LexError::InvalidDigit), span };
        }
        if digits.is_empty() {
            // `0x` or `0b` with no digits.
            return Token { kind: TokenKind::Error(LexError::InvalidDigit), span };
        }
        match u64::from_str_radix(&digits, radix) {
            Ok(v) => Token { kind: TokenKind::Int(v), span },
            Err(_) => Token { kind: TokenKind::Error(LexError::IntOverflow), span },
        }
    }

    fn read_escape(&mut self) -> Result<char, LexError> {
        let c = self.bump().ok_or(LexError::BadEscape)?;
        match c {
            'n' => Ok('\n'),
            'r' => Ok('\r'),
            't' => Ok('\t'),
            '\\' => Ok('\\'),
            '\'' => Ok('\''),
            '"' => Ok('"'),
            '0' => Ok('\0'),
            'x' => {
                let h1 = self.bump().ok_or(LexError::BadEscape)?;
                let h2 = self.bump().ok_or(LexError::BadEscape)?;
                let d1 = h1.to_digit(16).ok_or(LexError::BadEscape)?;
                let d2 = h2.to_digit(16).ok_or(LexError::BadEscape)?;
                let v = (d1 * 16 + d2) as u8;
                if v > 0x7F {
                    return Err(LexError::BadEscape);
                }
                Ok(v as char)
            }
            _ => Err(LexError::BadEscape),
        }
    }

    fn scan_char(&mut self, start: Mark) -> Token {
        self.bump(); // opening '

        // Empty: ''
        if self.peek() == Some('\'') {
            self.bump();
            return Token { kind: TokenKind::Error(LexError::EmptyChar), span: self.span_from(start) };
        }

        let value = match self.peek() {
            None | Some('\n') => {
                return Token {
                    kind: TokenKind::Error(LexError::UnterminatedChar),
                    span: self.span_from(start),
                };
            }
            Some('\\') => {
                self.bump();
                match self.read_escape() {
                    Ok(c) => c,
                    Err(e) => {
                        // Recover to next ', newline, or EOF.
                        while let Some(c) = self.peek() {
                            if c == '\'' {
                                self.bump();
                                break;
                            }
                            if c == '\n' {
                                break;
                            }
                            self.bump();
                        }
                        return Token { kind: TokenKind::Error(e), span: self.span_from(start) };
                    }
                }
            }
            Some(_) => self.bump().unwrap(),
        };

        if self.peek() == Some('\'') {
            self.bump();
            Token { kind: TokenKind::Char(value), span: self.span_from(start) }
        } else {
            // Too many chars or no closing quote — recover.
            while let Some(c) = self.peek() {
                if c == '\'' {
                    self.bump();
                    break;
                }
                if c == '\n' {
                    break;
                }
                self.bump();
            }
            Token { kind: TokenKind::Error(LexError::UnterminatedChar), span: self.span_from(start) }
        }
    }

    fn scan_string(&mut self, start: Mark, tokens: &mut Vec<Token>) {
        self.bump(); // opening "
        let mut value = String::new();
        loop {
            match self.peek() {
                None => {
                    tokens.push(Token {
                        kind: TokenKind::Error(LexError::UnterminatedString),
                        span: self.span_from(start),
                    });
                    return;
                }
                Some('\n') => {
                    // Multi-line strings not supported in v0.
                    tokens.push(Token {
                        kind: TokenKind::Error(LexError::UnterminatedString),
                        span: self.span_from(start),
                    });
                    return;
                }
                Some('"') => {
                    self.bump();
                    tokens.push(Token {
                        kind: TokenKind::Str(value),
                        span: self.span_from(start),
                    });
                    return;
                }
                Some('\\') => {
                    let esc_start = self.mark();
                    self.bump();
                    match self.read_escape() {
                        Ok(c) => value.push(c),
                        Err(e) => {
                            let span = self.span_from(esc_start);
                            tokens.push(Token { kind: TokenKind::Error(e), span });
                        }
                    }
                }
                Some(_) => {
                    let c = self.bump().unwrap();
                    value.push(c);
                }
            }
        }
    }

    fn scan_op_or_unexpected(&mut self, start: Mark, c: char) -> Token {
        use TokenKind::*;

        // 3-char ops
        if let (Some(c2), Some(c3)) = (self.peek2(), self.peek_at(2)) {
            let kind3 = match (c, c2, c3) {
                ('<', '<', '=') => Some(ShlEq),
                ('>', '>', '=') => Some(ShrEq),
                ('.', '.', '.') => Some(DotDotDot),
                _ => None,
            };
            if let Some(kind) = kind3 {
                self.bump();
                self.bump();
                self.bump();
                return Token { kind, span: self.span_from(start) };
            }
        }

        // 2-char ops
        if let Some(c2) = self.peek2() {
            let kind2 = match (c, c2) {
                ('=', '=') => Some(EqEq),
                ('!', '=') => Some(Ne),
                ('<', '=') => Some(Le),
                ('>', '=') => Some(Ge),
                ('&', '&') => Some(AndAnd),
                ('|', '|') => Some(OrOr),
                ('<', '<') => Some(Shl),
                ('>', '>') => Some(Shr),
                ('-', '>') => Some(Arrow),
                (':', ':') => Some(ColonColon),
                ('.', '.') => Some(DotDot),
                ('+', '=') => Some(PlusEq),
                ('-', '=') => Some(MinusEq),
                ('*', '=') => Some(StarEq),
                ('/', '=') => Some(SlashEq),
                ('%', '=') => Some(PercentEq),
                ('&', '=') => Some(AmpEq),
                ('|', '=') => Some(PipeEq),
                ('^', '=') => Some(CaretEq),
                _ => None,
            };
            if let Some(kind) = kind2 {
                self.bump();
                self.bump();
                return Token { kind, span: self.span_from(start) };
            }
        }

        // 1-char ops & punctuation
        let kind1 = match c {
            '(' => Some(LParen),
            ')' => Some(RParen),
            '{' => Some(LBrace),
            '}' => Some(RBrace),
            '[' => Some(LBracket),
            ']' => Some(RBracket),
            ',' => Some(Comma),
            ';' => Some(Semi),
            ':' => Some(Colon),
            '.' => Some(Dot),
            '+' => Some(Plus),
            '-' => Some(Minus),
            '*' => Some(Star),
            '/' => Some(Slash),
            '%' => Some(Percent),
            '=' => Some(Eq),
            '<' => Some(Lt),
            '>' => Some(Gt),
            '!' => Some(Bang),
            '&' => Some(Amp),
            '|' => Some(Pipe),
            '^' => Some(Caret),
            '~' => Some(Tilde),
            _ => None,
        };
        if let Some(kind) = kind1 {
            self.bump();
            return Token { kind, span: self.span_from(start) };
        }

        // Unrecognised: emit error and consume one char to make progress.
        self.bump();
        Token { kind: Error(LexError::UnexpectedChar(c)), span: self.span_from(start) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(src: &str) -> Vec<Token> {
        Lexer::new(src, FileId(0)).lex()
    }

    #[test]
    fn utf16_col_advance_for_supplementary_plane() {
        // '😀' is U+1F600 — outside BMP, needs a surrogate pair (UTF-16 len 2).
        let toks = lex("\"😀\"x");
        // Expect: Str("😀"), Ident("x"), Eof.
        assert_eq!(toks.len(), 3);
        // After the string token (2-char emoji + 2 quotes), the next token's
        // lsp_start.character should be 4 (1 + 2 + 1).
        let ident_lsp = &toks[1].span.lsp_start;
        assert_eq!(ident_lsp.line, 0);
        assert_eq!(ident_lsp.character, 4);
    }

    #[test]
    fn x_escape_round_trip() {
        let toks = lex(r#""\x41\x7F""#);
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[0].kind, TokenKind::Str("A\x7F".to_string()));
    }

    #[test]
    fn nested_block_comment() {
        let toks = lex("/* outer /* inner */ still outer */ x");
        // Expect just Ident("x") and Eof.
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[0].kind, TokenKind::Ident("x".to_string()));
        assert_eq!(toks[1].kind, TokenKind::Eof);
    }
}
