//! Performance harness — mirrors `../desktop/perf-harness` methodology: exercise
//! the hot paths at REAL scale (1000s of files, large docs) and time them.
//! `--perf` (see main.rs) also times offscreen render frames. The `#[test]`
//! below is a regression guard against O(N²) blowups in the logic hot paths.

use std::time::Instant;

use crate::vault::{markdown, tree};

/// A large markdown document (`lines` source lines of mixed constructs).
pub fn big_doc(lines: usize) -> String {
    let mut s = String::with_capacity(lines * 40);
    for i in 0..lines {
        match i % 8 {
            0 => s.push_str(&format!("# Section {i}\n")),
            1 => s.push_str(&format!(
                "A paragraph with **bold {i}**, *italic*, and `code` plus a [link](https://x/{i}).\n"
            )),
            2 => s.push_str(&format!("- bullet item number {i}\n")),
            3 => s.push_str(&format!("- [{}] task {i}\n", if i % 2 == 0 { "x" } else { " " })),
            4 => s.push_str(&format!("> a quoted line {i}\n")),
            5 => s.push_str(&format!("{}. ordered item {i}\n", i % 9 + 1)),
            6 => s.push('\n'),
            _ => s.push_str(&format!("plain trailing text for line {i}\n")),
        }
    }
    s
}

/// `n` files spread across nested folders (path, is_dir).
pub fn many_files(n: usize) -> Vec<(String, bool)> {
    let mut v = Vec::with_capacity(n + 100);
    for d in 0..(n / 50 + 1) {
        v.push((format!("dir{d:03}"), true));
    }
    for i in 0..n {
        v.push((format!("dir{:03}/note-{i}.md", i / 50), false));
    }
    v
}

/// Time the logic hot paths. Returns (markdown_parse_ms, tree_build_flatten_ms).
pub fn time_logic(doc_lines: usize, files: usize, iters: usize) -> (f64, f64) {
    let doc = big_doc(doc_lines);
    let t0 = Instant::now();
    for _ in 0..iters {
        let parsed = markdown::parse(&doc);
        std::hint::black_box(&parsed);
    }
    let parse_ms = t0.elapsed().as_secs_f64() * 1000.0 / iters as f64;

    let fs = many_files(files);
    let expanded: std::collections::HashMap<String, bool> = fs
        .iter()
        .filter(|(_, d)| *d)
        .map(|(p, _)| (p.clone(), true))
        .collect();
    let t1 = Instant::now();
    for _ in 0..iters {
        let nodes = tree::build_tree(fs.iter().map(|(p, d)| (p.as_str(), *d)));
        let flat = tree::flatten(&nodes, &expanded);
        std::hint::black_box(&flat);
    }
    let tree_ms = t1.elapsed().as_secs_f64() * 1000.0 / iters as f64;

    (parse_ms, tree_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logic_hot_paths_scale_linearly() {
        // Real scale: an 8k-line doc and 2k files. Generous thresholds (debug
        // build) — the point is to catch accidental O(N²)/quadratic regressions,
        // not to benchmark. Run `--perf` (release) for real numbers.
        let (parse_ms, tree_ms) = time_logic(8000, 2000, 5);
        assert!(parse_ms < 400.0, "markdown parse too slow: {parse_ms:.1}ms for 8k lines");
        assert!(tree_ms < 400.0, "tree build+flatten too slow: {tree_ms:.1}ms for 2k files");
    }
}
