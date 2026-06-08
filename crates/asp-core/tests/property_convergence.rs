//! Randomized convergence property (the headline determinism gate, fuzzed). For
//! many seeds: spin up N in-memory engines in one vault, apply a random stream of
//! create/edit/delete/rename ops across them, then deliver **every** row to
//! **every** engine in an independently-shuffled order — and assert all engines
//! converge to byte-identical materialized state. If the fold weren't a
//! deterministic, order-independent function of the row set, this would diverge.

use asp_core::{Identity, MemEngine, WireRow};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

fn run(seed: u64, n_engines: usize, n_ops: usize) {
    let mut rng = StdRng::seed_from_u64(seed);
    let engines: Vec<MemEngine> =
        (0..n_engines).map(|i| MemEngine::create(Identity::from_seed(&[(i as u8) + 1; 32]), "v")).collect();
    // A small shared path space → lots of contention (concurrent same-path edits,
    // renames into occupied paths, delete-vs-edit races).
    let paths = ["a.md", "b.md", "dir/c.md", "dir/d.txt", "code/x.rs"];

    let mut all_rows: Vec<WireRow> = Vec::new();
    for _ in 0..n_ops {
        let e = &engines[rng.gen_range(0..n_engines)];
        let p = paths[rng.gen_range(0..paths.len())];
        let authored = match rng.gen_range(0..5u8) {
            0..=2 => {
                let body = format!("line1\nval-{}\nline3\n", rng.gen_range(0..6));
                e.record_write(p, body.as_bytes())
            }
            3 => e.record_remove(p),
            _ => {
                let q = paths[rng.gen_range(0..paths.len())];
                e.record_rename(p, q)
            }
        }
        .expect("author");
        if let Some(wr) = authored {
            all_rows.push(wr);
        }
    }

    // Full mesh: deliver every row to every engine, each in its own shuffled order.
    for e in &engines {
        let mut order: Vec<usize> = (0..all_rows.len()).collect();
        // Fisher–Yates with this run's rng.
        for i in (1..order.len()).rev() {
            order.swap(i, rng.gen_range(0..=i));
        }
        for &idx in &order {
            let _ = e.integrate(&all_rows[idx]);
        }
    }

    // All engines hold the same rows → identical materialized state.
    let base = engines[0].files_map().expect("files");
    for (i, e) in engines.iter().enumerate().skip(1) {
        let other = e.files_map().expect("files");
        assert_eq!(other, base, "seed {seed}: engine {i} diverged from engine 0");
    }
}

#[test]
fn random_streams_converge_across_shuffled_delivery() {
    for seed in 0..300 {
        run(seed, 3, 25);
    }
}

#[test]
fn larger_meshes_converge() {
    for seed in 0..60 {
        run(seed, 5, 60);
    }
}
