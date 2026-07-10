#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffViewMode {
    Stack,
    Split,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffLineKind {
    Context,
    Addition,
    Deletion,
    Meta,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiffHunk {
    pub header: String,
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiffFile {
    pub old_path: String,
    pub new_path: String,
    pub status: Option<String>,
    pub additions: usize,
    pub deletions: usize,
    pub binary: bool,
    pub hunks: Vec<DiffHunk>,
}

impl DiffFile {
    pub fn display_path(&self) -> &str {
        if self.new_path.is_empty() || self.new_path == "/dev/null" {
            &self.old_path
        } else {
            &self.new_path
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiffDocument {
    pub files: Vec<DiffFile>,
}

impl DiffDocument {
    pub fn parse(input: &str) -> Self {
        let mut document = Self::default();
        let mut file: Option<DiffFile> = None;
        let mut hunk: Option<DiffHunk> = None;
        let mut old_line = 0;
        let mut new_line = 0;

        for line in input.lines() {
            if let Some(paths) = line.strip_prefix("diff --git ") {
                flush_hunk(&mut file, &mut hunk);
                flush_file(&mut document, &mut file);
                let (old_path, new_path) = parse_diff_paths(paths);
                file = Some(DiffFile {
                    old_path,
                    new_path,
                    ..DiffFile::default()
                });
                continue;
            }

            if let Some(header) = line.strip_prefix("@@") {
                flush_hunk(&mut file, &mut hunk);
                let full_header = format!("@@{header}");
                let (parsed_old_start, old_count, parsed_new_start, new_count) =
                    parse_hunk_header(&full_header).unwrap_or((0, 0, 0, 0));
                old_line = parsed_old_start;
                new_line = parsed_new_start;
                hunk = Some(DiffHunk {
                    header: full_header,
                    old_start: parsed_old_start,
                    old_count,
                    new_start: parsed_new_start,
                    new_count,
                    ..DiffHunk::default()
                });
                continue;
            }

            let Some(current_file) = file.as_mut() else {
                continue;
            };

            if let Some(current_hunk) = hunk.as_mut() {
                let (kind, text, old_number, new_number) =
                    if let Some(text) = line.strip_prefix('+') {
                        let number = new_line;
                        new_line += 1;
                        current_file.additions += 1;
                        (DiffLineKind::Addition, text, None, Some(number))
                    } else if let Some(text) = line.strip_prefix('-') {
                        let number = old_line;
                        old_line += 1;
                        current_file.deletions += 1;
                        (DiffLineKind::Deletion, text, Some(number), None)
                    } else if let Some(text) = line.strip_prefix(' ') {
                        let old_number = old_line;
                        let new_number = new_line;
                        old_line += 1;
                        new_line += 1;
                        (
                            DiffLineKind::Context,
                            text,
                            Some(old_number),
                            Some(new_number),
                        )
                    } else {
                        (DiffLineKind::Meta, line, None, None)
                    };
                current_hunk.lines.push(DiffLine {
                    kind,
                    old_line: old_number,
                    new_line: new_number,
                    text: text.to_owned(),
                });
                continue;
            }

            if let Some(path) = line.strip_prefix("--- ") {
                current_file.old_path = clean_path(path);
            } else if let Some(path) = line.strip_prefix("+++ ") {
                current_file.new_path = clean_path(path);
            } else if line.starts_with("new file mode ") {
                current_file.status = Some("Added".to_owned());
            } else if line.starts_with("deleted file mode ") {
                current_file.status = Some("Deleted".to_owned());
            } else if line.starts_with("rename from ") {
                current_file.status = Some("Renamed".to_owned());
            } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
                current_file.binary = true;
                current_file
                    .status
                    .get_or_insert_with(|| "Binary".to_owned());
            }
        }

        flush_hunk(&mut file, &mut hunk);
        flush_file(&mut document, &mut file);
        document
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplitRow {
    pub left: Option<DiffLine>,
    pub right: Option<DiffLine>,
}

pub fn split_rows(hunk: &DiffHunk) -> Vec<SplitRow> {
    let mut rows = Vec::new();
    let mut index = 0;

    while index < hunk.lines.len() {
        let line = &hunk.lines[index];
        match line.kind {
            DiffLineKind::Context => {
                rows.push(SplitRow {
                    left: Some(line.clone()),
                    right: Some(line.clone()),
                });
                index += 1;
            }
            DiffLineKind::Meta => {
                rows.push(SplitRow {
                    left: Some(line.clone()),
                    right: Some(line.clone()),
                });
                index += 1;
            }
            DiffLineKind::Addition | DiffLineKind::Deletion => {
                let start = index;
                while index < hunk.lines.len()
                    && matches!(
                        hunk.lines[index].kind,
                        DiffLineKind::Addition | DiffLineKind::Deletion
                    )
                {
                    index += 1;
                }
                let changed = &hunk.lines[start..index];
                let deletions = changed
                    .iter()
                    .filter(|line| line.kind == DiffLineKind::Deletion)
                    .cloned()
                    .collect::<Vec<_>>();
                let additions = changed
                    .iter()
                    .filter(|line| line.kind == DiffLineKind::Addition)
                    .cloned()
                    .collect::<Vec<_>>();
                for pair_index in 0..deletions.len().max(additions.len()) {
                    rows.push(SplitRow {
                        left: deletions.get(pair_index).cloned(),
                        right: additions.get(pair_index).cloned(),
                    });
                }
            }
        }
    }

    rows
}

fn flush_hunk(file: &mut Option<DiffFile>, hunk: &mut Option<DiffHunk>) {
    if let (Some(file), Some(hunk)) = (file.as_mut(), hunk.take()) {
        file.hunks.push(hunk);
    }
}

fn flush_file(document: &mut DiffDocument, file: &mut Option<DiffFile>) {
    if let Some(file) = file.take() {
        document.files.push(file);
    }
}

fn parse_diff_paths(paths: &str) -> (String, String) {
    let paths = paths.trim();
    if let Some((old_path, new_path)) = paths.split_once(" b/") {
        return (clean_path(old_path), clean_path(&format!("b/{new_path}")));
    }
    (String::new(), String::new())
}

fn clean_path(path: &str) -> String {
    let path = path.trim().trim_matches('"');
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
        .to_owned()
}

fn parse_hunk_header(header: &str) -> Option<(u32, u32, u32, u32)> {
    let ranges = header.strip_prefix("@@ ")?.split(" @@").next()?;
    let mut parts = ranges.split_whitespace();
    let old = parse_range(parts.next()?.strip_prefix('-')?)?;
    let new = parse_range(parts.next()?.strip_prefix('+')?)?;
    Some((old.0, old.1, new.0, new.1))
}

fn parse_range(range: &str) -> Option<(u32, u32)> {
    let (start, count) = range.split_once(',').unwrap_or((range, "1"));
    Some((start.parse().ok()?, count.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::{DiffDocument, DiffLineKind, split_rows};

    const SAMPLE: &str = r#"diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,4 +10,5 @@ fn example() {
 unchanged
-old one
-old two
+new one
+new two
+new three
 tail
"#;

    #[test]
    fn parses_files_hunks_stats_and_line_numbers() {
        let document = DiffDocument::parse(SAMPLE);
        assert_eq!(document.files.len(), 1);
        let file = &document.files[0];
        assert_eq!(file.display_path(), "src/lib.rs");
        assert_eq!((file.additions, file.deletions), (3, 2));
        assert_eq!(file.hunks.len(), 1);
        let lines = &file.hunks[0].lines;
        assert_eq!(lines[0].kind, DiffLineKind::Context);
        assert_eq!((lines[0].old_line, lines[0].new_line), (Some(10), Some(10)));
        assert_eq!((lines[1].old_line, lines[1].new_line), (Some(11), None));
        assert_eq!((lines[3].old_line, lines[3].new_line), (None, Some(11)));
        assert_eq!((lines[6].old_line, lines[6].new_line), (Some(13), Some(14)));
    }

    #[test]
    fn aligns_changed_blocks_for_split_view() {
        let document = DiffDocument::parse(SAMPLE);
        let rows = split_rows(&document.files[0].hunks[0]);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].left.as_ref().unwrap().text, "unchanged");
        assert_eq!(rows[1].left.as_ref().unwrap().text, "old one");
        assert_eq!(rows[1].right.as_ref().unwrap().text, "new one");
        assert!(rows[3].left.is_none());
        assert_eq!(rows[3].right.as_ref().unwrap().text, "new three");
        assert_eq!(rows[4].left.as_ref().unwrap().text, "tail");
    }

    #[test]
    fn recognizes_added_deleted_and_binary_files() {
        let document = DiffDocument::parse(
            "diff --git a/new.png b/new.png\nnew file mode 100644\nBinary files /dev/null and b/new.png differ\n\
             diff --git a/old.txt b/old.txt\ndeleted file mode 100644\n--- a/old.txt\n+++ /dev/null\n",
        );
        assert_eq!(document.files.len(), 2);
        assert!(document.files[0].binary);
        assert_eq!(document.files[0].status.as_deref(), Some("Added"));
        assert_eq!(document.files[1].status.as_deref(), Some("Deleted"));
        assert_eq!(document.files[1].display_path(), "old.txt");
    }

    #[test]
    fn parses_single_line_hunk_ranges() {
        let document =
            DiffDocument::parse("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -2 +2 @@\n-old\n+new\n");
        let hunk = &document.files[0].hunks[0];
        assert_eq!((hunk.old_start, hunk.old_count), (2, 1));
        assert_eq!((hunk.new_start, hunk.new_count), (2, 1));
    }
}
