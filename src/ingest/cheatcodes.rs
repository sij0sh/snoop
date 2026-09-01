//! Conditional chunking for cheatcodes knowledge corpora.
//!
//! `.agents/CHEATCODES.md` (written by the cheat-codes project) repeats one
//! pattern: an opening `<!-- cheatcodes-entry {...} -->` comment, the entry
//! body, and a closing `<!-- /cheatcodes-entry -->`. A chunk is the text
//! after one opening marker up to the next opening marker or end of file,
//! so an unclosed final entry is captured too.

use super::{markdown, units};
use crate::core::{BuiltUnit, SourceKind};

/// Opening-marker prefix. Does not match the closing marker, which reads
/// `<!-- /cheatcodes-entry`.
pub const ENTRY_MARKER: &str = "<!-- cheatcodes-entry";

/// Byte ranges (offset into `src`, slice) of each chunk: the leading
/// preamble when non-blank, then one chunk per opening marker. Closing
/// markers stay inside their chunk; the markdown parser ignores HTML blocks.
pub fn entry_chunks(src: &str) -> Vec<(usize, &str)> {
    let mut starts: Vec<usize> = src
        .match_indices(ENTRY_MARKER)
        .map(|(start, _)| start)
        .collect();
    if starts.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let first = starts[0];
    if !src[..first].trim().is_empty() {
        chunks.push((0, &src[..first]));
    }
    starts.push(src.len());
    for window in starts.windows(2) {
        chunks.push((window[0], &src[window[0]..window[1]]));
    }
    chunks
}

/// Chunker for the cheatcodes corpus locator; the ingest router calls this
/// only for `.agents/CHEATCODES.md`, so quoted markers in other markdown
/// never reach it and keep the standard heading-section strategy. Chunks
/// build units independently, so units never merge across entry
/// boundaries; offsets shift back onto the original file so source_slices
/// stay valid.
pub fn chunked_units(content: &str, title: &str, locator: &str) -> Vec<BuiltUnit> {
    let chunks = entry_chunks(content);
    if chunks.is_empty() {
        return units::build_units(
            &markdown::parse_markdown(content, title).atoms,
            SourceKind::Markdown,
            locator,
        );
    }
    let mut units = Vec::new();
    for (offset, chunk) in chunks {
        let mut atoms = markdown::parse_markdown(chunk, title).atoms;
        for atom in &mut atoms {
            atom.start_offset += offset;
            atom.end_offset += offset;
        }
        units.extend(units::build_units(&atoms, SourceKind::Markdown, locator));
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORPUS: &str = "# CHEATCODES\n\n\
        <!-- cheatcodes-entry {\"id\":\"a\"}-->\n## One\n\nBody one.\n\n\
        <!-- /cheatcodes-entry -->\n\
        <!-- cheatcodes-entry {\"id\":\"b\"}-->\n## Two\n\nBody two.\n\n\
        <!-- /cheatcodes-entry -->\n";

    #[test]
    fn splits_preamble_and_each_entry() {
        let chunks = entry_chunks(CORPUS);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], (0, "# CHEATCODES\n\n"));
        assert!(chunks[1].1.starts_with("<!-- cheatcodes-entry"));
        assert!(chunks[1].1.contains("## One"));
        assert!(chunks[2].1.contains("## Two"));
        assert!(!chunks[2].1.contains("Body one."));
        for (offset, chunk) in &chunks {
            assert_eq!(&CORPUS[*offset..*offset + chunk.len()], *chunk);
        }
    }

    #[test]
    fn last_chunk_runs_to_end_of_file_when_unclosed() {
        let src = "<!-- cheatcodes-entry {\"id\":\"a\"}-->\n## One\n\nBody.";
        let chunks = entry_chunks(src);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].1.len(), src.len());
    }

    #[test]
    fn marker_free_files_yield_no_chunks() {
        assert!(entry_chunks("# Just notes\n").is_empty());
        let closed = "<!-- /cheatcodes-entry -->\n";
        assert!(entry_chunks(closed).is_empty());
    }

    #[test]
    fn blank_preamble_yields_no_leading_chunk() {
        let src = "<!-- cheatcodes-entry {}-->\nbody\n";
        let chunks = entry_chunks(src);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, 0);
    }
}
