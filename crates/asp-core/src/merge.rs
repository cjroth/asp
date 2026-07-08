//! The one merge engine — 3-way merge against the last common ancestor, applied
//! by folding per-`file_id` diffs in fold order (§The merge model). What varies
//! by `merge_class` is the **conflict policy**, not the algorithm:
//!
//! - **Text** → clean-resolve: disjoint regions both survive; a same-region
//!   contention resolves to the later-in-fold-order side (`theirs`), the loser
//!   kept only in history. **No conflict markers.**
//! - **Code** → conflict-surface: same as text, but a true same-region conflict
//!   is rendered with **byte-deterministic** markers (`ASP:A` = ours = the
//!   earlier fold side, `ASP:B` = theirs = the later) for the agent to resolve.
//! - **Binary** → whole-file last-writer-wins (`theirs`) by fold order.
//!
//! `theirs` is always the operand later in the global fold order, so "theirs
//! wins" *is* the deterministic fold-order tiebreak, identical on every node.

use crate::log::{is_binary, MergeClass};
use std::collections::BTreeMap;

/// Split keeping line terminators so a join is byte-exact.
fn split_lines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = s.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'\n' {
            out.push(&s[start..=i]);
            start = i + 1;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

fn equal_pairs(base: &[&str], x: &[&str]) -> Vec<(usize, usize)> {
    use similar::{capture_diff_slices, Algorithm, DiffOp};
    let ops = capture_diff_slices(Algorithm::Myers, base, x);
    let mut pairs = Vec::new();
    for op in ops {
        if let DiffOp::Equal { old_index, new_index, len } = op {
            for k in 0..len {
                pairs.push((old_index + k, new_index + k));
            }
        }
    }
    pairs
}

/// Outcome of a single-file 3-way merge.
pub struct Merged {
    pub bytes: Vec<u8>,
    /// A same-region contention occurred (both sides changed the same region).
    /// For text this is surfaced only as a notification (output is clean); for
    /// code the output additionally carries conflict markers.
    pub conflict: bool,
}

/// Classic anchored diff3. `surface_code` selects code conflict-marker rendering
/// for true same-region conflicts; otherwise the later side (`theirs`) wins
/// cleanly.
fn diff3(base: &str, ours: &str, theirs: &str, surface_code: bool) -> (String, bool) {
    let b = split_lines(base);
    let o = split_lines(ours);
    let t = split_lines(theirs);

    let ma: BTreeMap<usize, usize> = equal_pairs(&b, &o).into_iter().collect();
    let mb: BTreeMap<usize, usize> = equal_pairs(&b, &t).into_iter().collect();

    let mut anchors: Vec<(usize, usize, usize)> = Vec::new();
    for (&bi, &oi) in &ma {
        if let Some(&ti) = mb.get(&bi) {
            anchors.push((bi, oi, ti));
        }
    }
    anchors.sort_unstable();

    let mut out = String::new();
    let mut conflict = false;
    let (mut pb, mut po, mut pt) = (0usize, 0usize, 0usize);

    let mut push_chunk = |out: &mut String, bc: &[&str], oc: &[&str], tc: &[&str]| {
        let bs: String = bc.concat();
        let os: String = oc.concat();
        let ts: String = tc.concat();
        if os == ts {
            out.push_str(&os);
        } else if os == bs {
            out.push_str(&ts); // ours unchanged → take theirs (incl. deletion)
        } else if ts == bs {
            out.push_str(&os); // theirs unchanged → take ours
        } else {
            // True conflict: both diverged from base in the same region.
            conflict = true;
            if surface_code {
                out.push_str("<<<<<<< ASP:A\n");
                out.push_str(&ensure_nl(&os));
                out.push_str("=======\n");
                out.push_str(&ensure_nl(&ts));
                out.push_str(">>>>>>> ASP:B\n");
            } else {
                out.push_str(&ts); // text clean-resolve: theirs wins, no markers
            }
        }
    };

    for &(bi, oi, ti) in &anchors {
        push_chunk(&mut out, &b[pb..bi], &o[po..oi], &t[pt..ti]);
        out.push_str(b[bi]); // agreed anchor line
        pb = bi + 1;
        po = oi + 1;
        pt = ti + 1;
    }
    push_chunk(&mut out, &b[pb..], &o[po..], &t[pt..]);
    (out, conflict)
}

fn ensure_nl(s: &str) -> String {
    if s.is_empty() || s.ends_with('\n') {
        s.to_string()
    } else {
        format!("{s}\n")
    }
}

/// 3-way merge of a single file's bytes under its `merge_class`.
pub fn merge3(class: MergeClass, base: &[u8], ours: &[u8], theirs: &[u8]) -> Merged {
    // Anything non-utf8/with NULs is treated as binary regardless of class.
    if class == MergeClass::Binary || is_binary(base) || is_binary(ours) || is_binary(theirs) {
        let conflict = ours != theirs && ours != base && theirs != base;
        return Merged { bytes: theirs.to_vec(), conflict };
    }
    let surface_code = class == MergeClass::Code;
    let (s, conflict) = diff3(
        std::str::from_utf8(base).unwrap(),
        std::str::from_utf8(ours).unwrap(),
        std::str::from_utf8(theirs).unwrap(),
        surface_code,
    );
    Merged { bytes: s.into_bytes(), conflict }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disjoint_text_edits_both_survive() {
        let base = "a\nb\nc\n";
        let ours = "A\nb\nc\n"; // changed line 1
        let theirs = "a\nb\nC\n"; // changed line 3
        let m = merge3(MergeClass::Text, base.as_bytes(), ours.as_bytes(), theirs.as_bytes());
        assert_eq!(String::from_utf8(m.bytes).unwrap(), "A\nb\nC\n");
        assert!(!m.conflict);
    }

    #[test]
    fn same_region_text_theirs_wins_no_markers() {
        let base = "x\n";
        let ours = "ours\n";
        let theirs = "theirs\n";
        let m = merge3(MergeClass::Text, base.as_bytes(), ours.as_bytes(), theirs.as_bytes());
        let out = String::from_utf8(m.bytes).unwrap();
        assert_eq!(out, "theirs\n");
        assert!(m.conflict);
        assert!(!out.contains("<<<<<<<"));
    }

    #[test]
    fn same_region_code_surfaces_markers_deterministically() {
        let base = "x\n";
        let ours = "ours\n";
        let theirs = "theirs\n";
        let m = merge3(MergeClass::Code, base.as_bytes(), ours.as_bytes(), theirs.as_bytes());
        let out = String::from_utf8(m.bytes).unwrap();
        assert!(m.conflict);
        assert_eq!(out, "<<<<<<< ASP:A\nours\n=======\ntheirs\n>>>>>>> ASP:B\n");
    }

    #[test]
    fn disjoint_code_edits_both_survive_no_markers() {
        let base = "def a():\n  pass\n\ndef b():\n  pass\n";
        let ours = "def a():\n  return 1\n\ndef b():\n  pass\n";
        let theirs = "def a():\n  pass\n\ndef b():\n  return 2\n";
        let m = merge3(MergeClass::Code, base.as_bytes(), ours.as_bytes(), theirs.as_bytes());
        let out = String::from_utf8(m.bytes).unwrap();
        assert!(!m.conflict, "got: {out}");
        assert_eq!(out, "def a():\n  return 1\n\ndef b():\n  return 2\n");
    }

    #[test]
    fn binary_is_last_writer_wins() {
        let m = merge3(MergeClass::Binary, &[0, 1], &[0, 2], &[0, 3]);
        assert_eq!(m.bytes, vec![0, 3]);
        assert!(m.conflict);
    }
}
