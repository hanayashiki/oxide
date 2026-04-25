use std::path::PathBuf;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct FileId(pub u32);

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
