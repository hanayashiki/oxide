use std::path::PathBuf;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Default)]
pub struct FileId(pub u32);

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
    pub file: FileId,
    pub start: BytePos,
    pub end: BytePos,
    pub lsp_start: LspPos,
    pub lsp_end: LspPos,
}

pub struct SourceFile {
    pub id: FileId,
    pub path: PathBuf,
    pub text: String,
}

#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, path: PathBuf, text: String) -> FileId {
        let id = FileId(self.files.len() as u32);
        self.files.push(SourceFile { id, path, text });
        id
    }

    pub fn get(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }
}
