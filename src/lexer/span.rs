#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct BytePos {
    pub offset: usize,
}

impl BytePos {
    pub const fn new(offset: usize) -> Self {
        Self { offset }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct LspPos {
    pub line: u32,
    pub character: u32,
}

impl LspPos {
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Span {
    pub start: BytePos,
    pub end: BytePos,
    pub lsp_start: LspPos,
    pub lsp_end: LspPos,
}

impl Span {
    pub const fn new(start: BytePos, end: BytePos, lsp_start: LspPos, lsp_end: LspPos) -> Self {
        Self { start, end, lsp_start, lsp_end }
    }
}
