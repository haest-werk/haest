//! Hashes that read the same in every build, on every machine, in every
//! release: FNV-1a over bytes the caller lays out itself.
//!
//! The standard library's `DefaultHasher` may change between Rust releases,
//! and `Hash` impls write integers in the machine's byte order. A hash that a
//! durable run writes down (an effect key handed to an outside API, an input
//! hash, the guard's repeat keys) must still match after a deploy built with
//! another toolchain, so these take bytes only.

const OFFSET_64: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME_64: u64 = 0x0000_0100_0000_01b3;
const OFFSET_128: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
const PRIME_128: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

/// Between two parts: a byte no UTF-8 text holds, so `("ab", "c")` and
/// `("a", "bc")` hash apart.
const SEPARATOR: u8 = 0xff;

/// 64-bit FNV-1a of `parts`, each followed by a separator.
pub(crate) fn hash64(parts: &[&[u8]]) -> u64 {
    let mut h = OFFSET_64;
    for byte in laid_out(parts) {
        h ^= u64::from(byte);
        h = h.wrapping_mul(PRIME_64);
    }
    h
}

/// 128-bit FNV-1a of `parts`, as 32 lowercase hex digits.
pub(crate) fn hex128(parts: &[&[u8]]) -> String {
    let mut h = OFFSET_128;
    for byte in laid_out(parts) {
        h ^= u128::from(byte);
        h = h.wrapping_mul(PRIME_128);
    }
    format!("{h:032x}")
}

fn laid_out<'a>(parts: &'a [&'a [u8]]) -> impl Iterator<Item = u8> + 'a {
    parts
        .iter()
        .flat_map(|p| p.iter().copied().chain([SEPARATOR]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned values: a change here breaks every effect key and input hash a
    /// durable run already wrote, so it must never happen by accident.
    #[test]
    fn hashes_are_pinned_across_builds() {
        // FNV-1a of the bytes 0x61 0xff, worked out apart from this code.
        assert_eq!(hash64(&[b"a"]), 0x089b_c907_b544_c769);
        assert_eq!(hex128(&[b"a"]), "0880954550ab1be95aa0733055b751b9");
    }

    #[test]
    fn parts_are_kept_apart() {
        assert_ne!(hash64(&[b"ab", b"c"]), hash64(&[b"a", b"bc"]));
        assert_ne!(hex128(&[b"ab", b"c"]), hex128(&[b"a", b"bc"]));
        assert_ne!(hex128(&[b"run", b"c1"]), hex128(&[b"run", b"c2"]));
    }
}
