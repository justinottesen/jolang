//! Source files and locations within them.

/// Identifies a source file within a `SourceMap`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileId(u32);

/// A range of bytes within a source file. `start` is inclusive and `end` is exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    /// Returns a span from the start of `self` to the end of `other`.
    pub fn to(self, other: Span) -> Span {
        Span { file: self.file, start: self.start, end: other.end }
    }
}

pub struct SourceFile {
    pub path: String,
    pub text: String,
    /// Byte offset of the start of each line. Always starts with 0.
    line_starts: Vec<u32>,
}

impl SourceFile {
    fn new(path: String, text: String) -> Self {
        let mut line_starts = vec![0];
        for (i, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        SourceFile { path, text, line_starts }
    }

    /// Returns the 1-based line and column of a byte offset. Columns count characters, not bytes.
    pub fn line_col(&self, offset: u32) -> (usize, usize) {
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let line_start = self.line_starts[line] as usize;
        let column = self.text[line_start..offset as usize].chars().count() + 1;
        (line + 1, column)
    }

    /// Returns the text of a 1-based line, without its line ending.
    pub fn line_text(&self, line: usize) -> &str {
        let start = self.line_starts[line - 1] as usize;
        let end = match self.line_starts.get(line) {
            Some(&next) => next as usize,
            None => self.text.len(),
        };
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }
}

/// Owns every source file in a compilation.
#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    /// Adds a file. Fails if the file is too large for its offsets to fit in a `u32`.
    pub fn add(&mut self, path: String, text: String) -> Result<FileId, String> {
        if u32::try_from(text.len()).is_err() {
            return Err(format!("`{path}` is too large (maximum size is 4 GiB)"));
        }
        let id = FileId(self.files.len() as u32);
        self.files.push(SourceFile::new(path, text));
        Ok(id)
    }

    pub fn get(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    /// Returns the source text a span covers.
    pub fn text(&self, span: Span) -> &str {
        &self.get(span.file).text[span.start as usize..span.end as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col() {
        let mut sources = SourceMap::default();
        let id = sources.add("test.jo".into(), "ab\ncd\n\nef".into()).unwrap();
        let file = sources.get(id);
        assert_eq!(file.line_col(0), (1, 1));
        assert_eq!(file.line_col(1), (1, 2));
        assert_eq!(file.line_col(2), (1, 3));
        assert_eq!(file.line_col(3), (2, 1));
        assert_eq!(file.line_col(6), (3, 1));
        assert_eq!(file.line_col(7), (4, 1));
        assert_eq!(file.line_col(9), (4, 3));
    }

    #[test]
    fn line_text() {
        let mut sources = SourceMap::default();
        let id = sources.add("test.jo".into(), "ab\r\ncd\n\nef".into()).unwrap();
        let file = sources.get(id);
        assert_eq!(file.line_text(1), "ab");
        assert_eq!(file.line_text(2), "cd");
        assert_eq!(file.line_text(3), "");
        assert_eq!(file.line_text(4), "ef");
    }
}
