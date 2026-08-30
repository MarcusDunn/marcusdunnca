//! Seeded permutations.
//!
//! Two things need to move an answer around, for the same reason and with the
//! same requirement that it be reproducible:
//!
//!   - **generation**, once per document, because models have a positional bias
//!     they cannot introspect on — measured on one document, Sonnet keyed `b`
//!     nine times out of ten and never once used `d`;
//!   - **review**, once per repetition, because a question whose answer stays
//!     at `c` stops testing the fact after the second sitting and starts testing
//!     whether you remember the letter. That failure is worse than the first
//!     one: it is invisible, and it makes the review schedule accumulate
//!     evidence about nothing.
//!
//! Seeded rather than random so that a bug here is reproducible, and so that a
//! regenerated document produces the same quiz. Not cryptographic and does not
//! need to be — it decides where a quiz answer sits, and the quiz is taken by
//! the person who generated it.

/// A permutation of `0..len`, derived from `seed`.
///
/// Fisher-Yates, which is uniform. Repeatedly swapping random pairs is the
/// version that looks equivalent and is not.
pub fn permutation(len: usize, seed: &str) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    let mut state = fnv1a(seed);

    for i in (1..order.len()).rev() {
        let j = (next_u64(&mut state) % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }

    order
}

/// Apply a permutation, in place, to any slice.
///
/// `order[i]` is the index of the item that should end up at position `i`.
pub fn apply<T: Clone>(items: &[T], order: &[usize]) -> Vec<T> {
    order
        .iter()
        .filter_map(|&i| items.get(i).cloned())
        .collect()
}

/// FNV-1a. Any stable hash would do; this one is four lines and needs no
/// dependency.
pub fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    // xorshift64* requires a non-zero state, and FNV of the empty string is
    // non-zero, so this only guards a hash that happens to land on zero.
    if hash == 0 {
        0x9e37_79b9_7f4a_7c15
    } else {
        hash
    }
}

/// xorshift64*.
pub fn next_u64(state: &mut u64) -> u64 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_permutation_contains_every_index_exactly_once() {
        for len in 0..8 {
            let order = permutation(len, "seed");
            let mut sorted = order.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, (0..len).collect::<Vec<_>>(), "len {len}");
        }
    }

    #[test]
    fn the_same_seed_gives_the_same_permutation() {
        assert_eq!(permutation(4, "doc-1"), permutation(4, "doc-1"));
    }

    /// The property review depends on: consecutive repetitions of one question
    /// must not present the options the same way, or the reader learns the
    /// letter instead of the fact.
    #[test]
    fn different_seeds_generally_give_different_permutations() {
        let orders: Vec<Vec<usize>> = (0..8).map(|i| permutation(4, &format!("q1:{i}"))).collect();
        let distinct: std::collections::HashSet<&Vec<usize>> = orders.iter().collect();
        assert!(
            distinct.len() >= 4,
            "eight seeds produced only {} distinct orders: {orders:?}",
            distinct.len()
        );
    }

    #[test]
    fn apply_reorders_without_losing_anything() {
        let items = ["a", "b", "c", "d"];
        let order = permutation(4, "seed");
        let moved = apply(&items, &order);
        assert_eq!(moved.len(), 4);
        let mut sorted = moved.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec!["a", "b", "c", "d"]);
    }
}
