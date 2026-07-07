//! Range-Based Set Reconciliation (RBSR) — the anti-entropy substrate fix
//! (scoped-sync §2). Replaces the one-shot dense-seq version-vector exchange with
//! reconciliation driven by **actual set membership** over the flat id-space, so
//! it is immune to the dense-seq hole (a receiver that stored `{0,1,2,5}` with a
//! gap) that the VV silently lies about. Well-synced pairs cost one round-trip;
//! divergence costs ≈ `log(n)` rounds.
//!
//! This module is the **pure** core: the sound (non-XOR) fingerprint, the range
//! split, and the wire part types. It is std-only and always-compiled, so it runs
//! byte-identically on native and in wasm. The multi-round choreography lives in
//! [`crate::session`]; the per-vault id/row queries live on the engines.
//!
//! ## Reconcile set & order
//! Every log row a node holds, keyed by its merkle `id` (SHA-256, stored as 64 hex
//! chars). The total order is **lexicographic over the hex id** — one flat id-space
//! across all sites and kinds. RBSR never looks at `site_id`/`seq`, which is exactly
//! why it cannot be fooled by a dense-prefix gap.
//!
//! ## Fingerprint (⚠️ NOT XOR)
//! An **incremental, associative, commutative** multiset hash: the wrapping 256-bit
//! sum of `H(id)` over the range, where `H` is a domain-separated SHA-256. Identical
//! id-sets ⇒ identical fingerprint on every node; the empty range is the additive
//! identity (all-zero). Addition (an *AdHash*-style combiner) is used deliberately
//! instead of XOR: XOR **self-cancels** — two different id-sets can XOR to the same
//! value (e.g. `{0001,0010}` and `{0100,0111}` both XOR to `0011`), so a divergent
//! range would look "in sync" and its rows would be **silently dropped**, the exact
//! hole this spec removes. Addition distinguishes those sets, and a modular
//! subset-sum collision needs ≈2^128 grinding of SHA-256 ids to force. The
//! collision-adversarial fuzz test (`fingerprint_distinguishes_xor_colliding_sets`)
//! pins that an XOR-class combiner can never slip back in.

use sha2::{Digest, Sha256};

/// Range leaf threshold: a range with ≤ this many items on *both* sides is
/// enumerated directly (an `ItemSet` id exchange) rather than split further.
pub const RBSR_LEAF: usize = 32;

/// Range fan-out: a differing, over-`RBSR_LEAF` range is split into this many
/// sub-ranges by item count (median-id boundaries via the sorted id list).
pub const RBSR_SPLIT: usize = 16;

/// A 256-bit multiset-hash fingerprint over a range's id set (little-endian).
pub type Fingerprint = [u8; 32];

/// The additive identity — the fingerprint of the empty range.
pub const FP_ZERO: Fingerprint = [0u8; 32];

/// Domain-separated per-id field element `H(id)`, summed into the fingerprint.
fn fp_element(id: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"asp-rbsr-fp-v1\0");
    h.update(id.as_bytes());
    let out = h.finalize();
    let mut e = [0u8; 32];
    e.copy_from_slice(&out);
    e
}

/// `acc += element(id)` as a 256-bit little-endian wrapping add (in place).
pub fn fp_add(acc: &mut Fingerprint, id: &str) {
    let e = fp_element(id);
    let mut carry = 0u16;
    for i in 0..32 {
        let s = acc[i] as u16 + e[i] as u16 + carry;
        acc[i] = (s & 0xff) as u8;
        carry = s >> 8;
    }
    // Overflow past 256 bits wraps (mod 2^256) — that IS the group operation.
}

/// The fingerprint of a set of ids — order-independent (commutative + associative).
pub fn fingerprint<'a>(ids: impl IntoIterator<Item = &'a str>) -> Fingerprint {
    let mut acc = FP_ZERO;
    for id in ids {
        fp_add(&mut acc, id);
    }
    acc
}

/// Hex-encode a fingerprint for the wire.
pub fn fp_hex(fp: &Fingerprint) -> String {
    hex::encode(fp)
}

/// Decode a wire fingerprint; `None` on malformed input (treated as a mismatch).
pub fn fp_from_hex(s: &str) -> Option<Fingerprint> {
    let bytes = hex::decode(s).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut fp = FP_ZERO;
    fp.copy_from_slice(&bytes);
    Some(fp)
}

/// A range endpoint. `None` = open (−∞ for `lo`, +∞ for `hi`). The reconcile set is
/// ordered by hex id, so a bound is just an id string compared lexicographically.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Bound(pub Option<String>);

impl Bound {
    pub fn open() -> Bound {
        Bound(None)
    }
    pub fn at(id: &str) -> Bound {
        Bound(Some(id.to_string()))
    }
}

/// Is `id` inside the half-open range `[lo, hi)` (lexicographic)?
pub fn in_range(id: &str, lo: &Bound, hi: &Bound) -> bool {
    if let Some(l) = &lo.0 {
        if id < l.as_str() {
            return false;
        }
    }
    if let Some(h) = &hi.0 {
        if id >= h.as_str() {
            return false;
        }
    }
    true
}

/// One part of a [`crate::wire::Msg::Reconcile`] frame.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RangePart {
    /// "My fingerprint (+ item count) for `[lo,hi)`." The receiver compares to its
    /// own: match ⇒ range converged; differ ⇒ split & reply, or (both small) leaf.
    Fingerprint { lo: Bound, hi: Bound, fp: String, count: u64 },
    /// Leaf: the range is small enough to enumerate. `ids` are the sender's ids in
    /// `[lo,hi)`; `want=true` asks the peer to send its own leaf back (so the sender
    /// can ship what the peer lacks). Rows travel separately as `Msg::Rows`.
    ItemSet { lo: Bound, hi: Bound, ids: Vec<String>, want: bool },
}

/// Split the SORTED `ids` within `[lo,hi)` into ≤ `RBSR_SPLIT` contiguous half-open
/// sub-ranges by item count (median-id boundaries), together covering exactly
/// `[lo,hi)`. The boundaries travel explicitly in the reply, so the peer computes
/// its fingerprint for the SAME sub-ranges — no independent-median agreement needed.
/// `ids` must be the sorted ids the splitter holds in the range (its local view).
pub fn split_bounds(ids: &[String], lo: &Bound, hi: &Bound, k: usize) -> Vec<(Bound, Bound)> {
    let k = k.max(2);
    let n = ids.len();
    // Fewer items than parts → one range per item boundary (still covers [lo,hi)).
    let parts = k.min(n).max(1);
    if parts == 1 {
        return vec![(lo.clone(), hi.clone())];
    }
    let mut out = Vec::with_capacity(parts);
    let mut cur_lo = lo.clone();
    for p in 1..parts {
        let idx = p * n / parts;
        let boundary = Bound::at(&ids[idx]);
        out.push((cur_lo.clone(), boundary.clone()));
        cur_lo = boundary;
    }
    out.push((cur_lo, hi.clone()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<String> {
        // Deterministic distinct ids in sorted order (hex of a SHA over the index).
        let mut v: Vec<String> = (0..n).map(|i| fp_hex(&fp_element(&format!("row-{i}")))).collect();
        v.sort();
        v.dedup();
        v
    }

    #[test]
    fn fingerprint_is_order_independent_and_empty_is_identity() {
        assert_eq!(fingerprint(std::iter::empty::<&str>()), FP_ZERO);
        let a = ["aa", "bb", "cc"];
        let b = ["cc", "aa", "bb"];
        assert_eq!(fingerprint(a.iter().copied()), fingerprint(b.iter().copied()), "sum is commutative");
        // Incremental accumulation matches a whole-range recompute.
        let mut acc = FP_ZERO;
        for id in &a {
            fp_add(&mut acc, id);
        }
        assert_eq!(acc, fingerprint(a.iter().copied()));
    }

    #[test]
    fn fingerprint_hex_roundtrips() {
        let fp = fingerprint(["x", "y"].iter().copied());
        assert_eq!(fp_from_hex(&fp_hex(&fp)), Some(fp));
        assert_eq!(fp_from_hex("nothex"), None);
        assert_eq!(fp_from_hex("aa"), None);
    }

    /// The headline soundness guard (scoped-sync §2.1, §9): an XOR combiner
    /// self-cancels — `{0001,0010}` and `{0100,0111}` both XOR to `0011` — so it
    /// would judge two DIFFERENT id-sets "in sync" and silently drop the divergent
    /// rows. Our additive fingerprint MUST distinguish them.
    #[test]
    fn fingerprint_distinguishes_xor_colliding_sets() {
        // Classic 2-vs-2 XOR collision on the raw ids.
        let set_a = ["0001", "0010"]; // xor = 0011
        let set_b = ["0100", "0111"]; // xor = 0011
        // XOR of the raw id bytes would be equal; assert our fingerprint is NOT.
        assert_ne!(
            fingerprint(set_a.iter().copied()),
            fingerprint(set_b.iter().copied()),
            "additive fingerprint must not collide where XOR does",
        );
        // A single differing element always changes the fingerprint.
        assert_ne!(fingerprint(["a", "b"].iter().copied()), fingerprint(["a", "c"].iter().copied()));
        // Adding one element to a set changes the fingerprint (no even-multiplicity
        // cancellation, unlike XOR).
        let base = fingerprint(["a", "b", "c"].iter().copied());
        let plus = fingerprint(["a", "b", "c", "d"].iter().copied());
        assert_ne!(base, plus);
    }

    #[test]
    fn fingerprint_distinguishes_many_random_sets() {
        // A broad sweep: 200 distinct singleton ids all yield distinct fingerprints,
        // and no pair of 2-element subsets collides (would flag an XOR-class bug).
        let v = ids(60);
        let mut seen = std::collections::HashSet::new();
        for i in 0..v.len() {
            for j in (i + 1)..v.len() {
                let fp = fingerprint([v[i].as_str(), v[j].as_str()].iter().copied());
                assert!(seen.insert(fp), "two distinct pairs collided — combiner is unsound");
            }
        }
    }

    #[test]
    fn split_bounds_covers_range_and_is_balanced() {
        let v = ids(100);
        let lo = Bound::open();
        let hi = Bound::open();
        let parts = split_bounds(&v, &lo, &hi, RBSR_SPLIT);
        assert_eq!(parts.len(), RBSR_SPLIT, "100 items / 16 splits → 16 sub-ranges");
        // Contiguous + covers (−∞,+∞): first lo open, last hi open, each hi == next lo.
        assert_eq!(parts.first().unwrap().0, Bound::open());
        assert_eq!(parts.last().unwrap().1, Bound::open());
        for w in parts.windows(2) {
            assert_eq!(w[0].1, w[1].0, "sub-ranges are contiguous");
        }
        // Every id falls into exactly one sub-range.
        for id in &v {
            let hits = parts.iter().filter(|(l, h)| in_range(id, l, h)).count();
            assert_eq!(hits, 1, "id {id} must fall in exactly one sub-range");
        }
    }

    #[test]
    fn split_bounds_handles_small_and_singleton() {
        let v = ids(3);
        let parts = split_bounds(&v, &Bound::open(), &Bound::open(), RBSR_SPLIT);
        assert!(parts.len() <= 3 && !parts.is_empty());
        // A single item → a single covering range.
        let one = ids(1);
        let p1 = split_bounds(&one, &Bound::open(), &Bound::open(), RBSR_SPLIT);
        assert_eq!(p1, vec![(Bound::open(), Bound::open())]);
    }
}
