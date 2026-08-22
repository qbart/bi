//! One settle's worth of edits, composed into one `didChange` content change.
//!
//! The ranges are **whole lines, always at character 0** — the shape Neovim
//! has shipped for years. Both position encodings agree on column 0, so
//! document sync never converts a UTF-16 column; and the replacement text is
//! read from the rope *after* the batch, so no historical text is needed and
//! `Buffer::Edit` carries none. See `docs/specs/lsp.md`.

use ropey::Rope;

use super::types::{Position, Range};
use crate::buffer::Edit;

/// A batch's damage, as lines: pre-batch lines `[lo, old_hi)` became the
/// current rope's lines `[lo, new_hi)`. Lines above `lo` were untouched — so
/// pre-batch and current numbering agree there — and lines below map one to
/// one with an offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub lo: usize,
    pub old_hi: usize,
    pub new_hi: usize,
}

/// Folds a batch into one [`Span`]. `None` for an empty batch.
///
/// Each `Edit`'s rows are in the document as of just before that edit — the
/// same convention tree-sitter consumes them under — so the fold widens the
/// running span through each: rows of the new edit that fall below the span's
/// current extent name untouched lines and map straight back to pre-batch
/// numbering; rows inside it are already accounted for.
pub fn compose(edits: &[Edit]) -> Option<Span> {
    let mut span: Option<Span> = None;
    for e in edits {
        let (e_lo, e_old_hi, e_new_hi) =
            (e.start_point.row, e.old_end_point.row + 1, e.new_end_point.row + 1);
        span = Some(match span {
            None => Span { lo: e_lo, old_hi: e_old_hi, new_hi: e_new_hi },
            Some(s) => Span {
                lo: s.lo.min(e_lo),
                old_hi: s.old_hi + e_old_hi.saturating_sub(s.new_hi),
                // `max + e_new_hi ≥ e_old_hi` always, so the subtraction is
                // safe in usize even when the edit shrinks the document.
                new_hi: (s.new_hi.max(e_old_hi) + e_new_hi) - e_old_hi,
            },
        });
    }
    span
}

/// The wire change for a composed span: replace pre-batch lines
/// `[lo, old_hi)` with the current rope's lines `[lo, new_hi)`.
///
/// When the span reaches the end of the file, `old_hi` names the line one
/// past the pre-batch last — a position the spec defines as clamping, and the
/// exact shape every server already receives from Neovim.
pub fn change(rope: &Rope, span: Span) -> (Range, String) {
    let start = rope.line_to_char(span.lo.min(rope.len_lines()));
    let end = rope.line_to_char(span.new_hi.min(rope.len_lines()));
    let range = Range {
        start: Position { line: span.lo as u32, character: 0 },
        end: Position { line: span.old_hi as u32, character: 0 },
    };
    (range, rope.slice(start..end).to_string())
}

#[cfg(test)]
mod tests {
    use crate::buffer::{Buffer, Cursor};

    use super::*;

    /// Applies a produced change to the pre-batch text, with the clamping
    /// the spec requires of servers. This is the other half of the invariant:
    /// what a server would hold after the change must be the rope.
    fn apply(pre: &str, range: Range, text: &str) -> String {
        // Byte offsets where each line starts; one past the end stands for
        // every line number beyond the document, which is the clamp.
        let mut starts = vec![0];
        starts.extend(pre.char_indices().filter(|&(_, c)| c == '\n').map(|(i, _)| i + 1));
        let at = |line: u32| starts.get(line as usize).copied().unwrap_or(pre.len());
        format!("{}{}{}", &pre[..at(range.start.line)], text, &pre[at(range.end.line)..])
    }

    /// Runs `ops` as one settle batch and checks the invariant.
    fn check(pre: &str, ops: &[(usize, usize, &str)]) {
        let mut buffer = Buffer::empty();
        if !pre.is_empty() {
            buffer.insert_str(Cursor::at(0), pre);
            buffer.pending_edits.clear();
        }
        for &(start, end, text) in ops {
            buffer.replace_range(start, end, text);
        }
        let edits = std::mem::take(&mut buffer.pending_edits);
        let span = compose(&edits).expect("ops is never empty");
        let (range, text) = change(buffer.rope(), span);
        assert_eq!(
            apply(pre, range, &text),
            buffer.rope().to_string(),
            "pre {pre:?}, ops {ops:?}, span {span:?}, change {range:?} {text:?}"
        );
    }

    #[test]
    fn one_insertion_replaces_its_own_line() {
        check("fn main() {\n}\n", &[(3, 3, "x")]);
    }

    #[test]
    fn an_insertion_with_a_newline_replaces_one_line_with_two() {
        check("ab\ncd\n", &[(1, 1, "X\nY")]);
    }

    #[test]
    fn a_deletion_across_lines_composes() {
        check("one\ntwo\nthree\n", &[(2, 9, "")]);
    }

    #[test]
    fn edits_on_separate_lines_compose_into_one_span() {
        // Two cursors' worth: row 0 and row 2, one batch.
        check("aaa\nbbb\nccc\n", &[(0, 1, "X"), (8, 9, "Y")]);
    }

    #[test]
    fn a_second_edit_above_the_first_extends_the_span_up() {
        check("aaa\nbbb\nccc\n", &[(8, 9, "Y"), (0, 1, "X")]);
    }

    #[test]
    fn deleting_what_an_earlier_edit_inserted_still_matches() {
        // Edit 2's rows overlap edit 1's damage — the case that rules out
        // computing per-edit text from the final rope.
        check("ab\n", &[(1, 1, "X\nY"), (1, 4, "")]);
    }

    #[test]
    fn appending_at_the_end_of_a_file_without_a_final_newline() {
        // `old_hi` names the line one past the last — the clamped shape.
        check("a", &[(1, 1, "b")]);
    }

    #[test]
    fn deleting_the_final_newline_shrinks_the_line_count() {
        check("a\n", &[(1, 2, "")]);
    }

    #[test]
    fn editing_the_empty_document() {
        check("", &[(0, 0, "hello\n")]);
    }

    #[test]
    fn deleting_everything() {
        check("one\ntwo\n", &[(0, 8, "")]);
    }

    #[test]
    fn an_edit_below_then_a_big_deletion_above_keeps_the_lines_below_aligned() {
        // Ten lines; split row 8, then delete rows 0..6. The line below the
        // span must map back with the right offset — the case the fold's
        // `saturating_sub` exists for.
        let pre = "0\n1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        check(pre, &[(16, 16, "x\ny"), (0, 12, "z")]);
    }

    #[test]
    fn a_seeded_sweep_of_random_batches_holds_the_invariant() {
        // A linear congruential generator, not `rand`: the crate has no
        // randomness dependency and a fixed seed keeps failures replayable.
        let mut state: u64 = 0x2545F4914F6CDD1D;
        let mut next = move |bound: usize| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((state >> 33) as usize) % bound.max(1)
        };
        let alphabet = ['a', 'b', '\n', 'é', ' '];

        for _ in 0..300 {
            // A pre-document of 0..40 chars.
            let pre: String = (0..next(40)).map(|_| alphabet[next(alphabet.len())]).collect();
            let mut buffer = Buffer::empty();
            if !pre.is_empty() {
                buffer.insert_str(Cursor::at(0), &pre);
                buffer.pending_edits.clear();
            }

            // A batch of 1..5 replacements at random spots.
            for _ in 0..1 + next(4) {
                let len = buffer.rope().len_chars();
                let start = next(len + 1);
                let end = (start + next(len - start + 1)).min(len);
                let text: String = (0..next(6)).map(|_| alphabet[next(alphabet.len())]).collect();
                if start == end && text.is_empty() {
                    continue;
                }
                buffer.replace_range(start, end, &text);
            }

            let edits = std::mem::take(&mut buffer.pending_edits);
            let Some(span) = compose(&edits) else { continue };
            let (range, text) = change(buffer.rope(), span);
            assert_eq!(
                apply(&pre, range, &text),
                buffer.rope().to_string(),
                "pre {pre:?}, edits {edits:?}"
            );
        }
    }
}
