#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum LexError {
    UnexpectedChar(char),
    UnterminatedBlockComment,
    UnterminatedString,
    UnterminatedChar,
    EmptyChar,
    BadEscape,
    IntOverflow,
    InvalidDigit,
}
