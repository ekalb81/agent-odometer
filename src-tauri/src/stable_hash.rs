/// Stable FNV-1a 64-bit fold for persisted local identities.
pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::fnv1a64;

    #[test]
    fn frozen_fnv1a64_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a64(b"hello"), 0xa430d84680aabd0b);
        assert_eq!(fnv1a64("é".as_bytes()), 0x0ac21707b7181e01);
    }
}
