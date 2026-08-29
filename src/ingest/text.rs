use crate::core::{AtomKind, ParsedAtom};

pub fn parse_text(src: &str, title: &str) -> Vec<ParsedAtom> {
    let mut atoms = vec![ParsedAtom {
        kind: AtomKind::Document,
        parent_index: None,
        ordinal: 0,
        start_offset: 0,
        end_offset: src.len(),
        text: src.to_string(),
        breadcrumb: title.to_string(),
        content_hash: ParsedAtom::content_hash_of(AtomKind::Document, title, src),
        metadata: serde_json::Value::Null,
    }];

    let mut ordinal = 1;
    let mut offset = 0;
    let mut paragraph_start = None;
    let mut paragraph_end = 0;
    let flush =
        |start: Option<usize>, end: usize, atoms: &mut Vec<ParsedAtom>, ordinal: &mut u32| {
            let Some(start) = start.filter(|start| end > *start) else {
                return;
            };
            let text = &src[start..end];
            if text.trim().is_empty() {
                return;
            }
            atoms.push(ParsedAtom {
                kind: AtomKind::Paragraph,
                parent_index: Some(0),
                ordinal: *ordinal,
                start_offset: start,
                end_offset: end,
                text: text.to_string(),
                breadcrumb: title.to_string(),
                content_hash: ParsedAtom::content_hash_of(AtomKind::Paragraph, title, text),
                metadata: serde_json::Value::Null,
            });
            *ordinal += 1;
        };

    for segment in src.split_inclusive('\n') {
        let line_start = offset;
        offset += segment.len();
        let line = segment.trim_end_matches(['\r', '\n']);
        let line_end = line_start + line.len();
        if line.trim().is_empty() {
            flush(
                paragraph_start.take(),
                paragraph_end,
                &mut atoms,
                &mut ordinal,
            );
        } else {
            if paragraph_start.is_none() {
                paragraph_start = Some(line_start);
            }
            paragraph_end = line_end;
        }
    }
    flush(paragraph_start, paragraph_end, &mut atoms, &mut ordinal);
    atoms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_offsets_round_trip() {
        let source = "first\r\nline\r\n\r\nsecond\r\n";
        let atoms = parse_text(source, "notes");
        for atom in atoms.iter().filter(|atom| atom.kind == AtomKind::Paragraph) {
            assert_eq!(&source[atom.start_offset..atom.end_offset], atom.text);
        }
        assert_eq!(atoms[1].text, "first\r\nline");
        assert_eq!(atoms[2].text, "second");
    }
}
