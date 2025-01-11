mod mem;
pub use mem::InMemoryMMR;

use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Hash<const N: usize>([u8; N]);

/// Interface the MMR uses for hashing a leaf element with its position and for generating the hash of a non-leaf node.
pub trait Hasher<const N: usize> {
    fn hash_leaf(&mut self, pos: u64, hash: &Hash<N>) -> Hash<N>;
    fn hash_node(&mut self, pos: u64, hash1: &Hash<N>, hash2: &Hash<N>) -> Hash<N>;
}

struct Sha256Hasher {
    hasher: Sha256,
}

impl Sha256Hasher {
    pub fn new() -> Self {
        Self {
            hasher: Sha256::new(),
        }
    }
}

impl Hasher<32> for Sha256Hasher {
    fn hash_leaf(&mut self, pos: u64, hash: &Hash<32>) -> Hash<32> {
        self.hasher.update(pos.to_be_bytes());
        self.hasher.update(hash.0);
        Hash(self.hasher.finalize_reset().into())
    }

    fn hash_node(&mut self, pos: u64, hash1: &Hash<32>, hash2: &Hash<32>) -> Hash<32> {
        self.hasher.update(pos.to_be_bytes());
        self.hasher.update(hash1.0);
        self.hasher.update(hash2.0);
        Hash(self.hasher.finalize_reset().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_leaf_sha256() {
        let mut hasher = Sha256Hasher::new();
        test_hash_leaf::<32, Sha256Hasher>(&mut hasher);
    }

    #[test]
    fn test_hash_node_sha256() {
        let mut hasher = Sha256Hasher::new();
        test_hash_node::<32, Sha256Hasher>(&mut hasher);
    }

    fn test_hash_leaf<const N: usize, H: Hasher<N>>(hasher: &mut H) {
        // input hashes to use
        let hash1 = Hash([1u8; N]);
        let hash2 = Hash([2u8; N]);

        let out = hasher.hash_leaf(0, &hash1);
        assert_ne!(out.0, [0u8; N], "hash should be non-zero");

        let mut out2 = hasher.hash_leaf(0, &hash1);
        assert_eq!(out, out2, "hash should be re-computed consistently");

        out2 = hasher.hash_leaf(1, &hash1);
        assert_ne!(out, out2, "hash should change with different pos");

        out2 = hasher.hash_leaf(0, &hash2);
        assert_ne!(out, out2, "hash should change with different input hash");
    }

    fn test_hash_node<const N: usize, H: Hasher<N>>(hasher: &mut H) {
        // input hashes to use
        let hash1 = Hash([1u8; N]);
        let hash2 = Hash([2u8; N]);
        let hash3 = Hash([3u8; N]);

        let out = hasher.hash_node(0, &hash1, &hash2);
        assert_ne!(out.0, [0u8; N], "hash should be non-zero");

        let mut out2 = hasher.hash_node(0, &hash1, &hash2);
        assert_eq!(out, out2, "hash should be re-computed consistently");

        out2 = hasher.hash_node(1, &hash1, &hash2);
        assert_ne!(out, out2, "hash should change with different pos");

        out2 = hasher.hash_node(0, &hash3, &hash2);
        assert_ne!(
            out, out2,
            "hash should change with different first input hash"
        );

        out2 = hasher.hash_node(0, &hash1, &hash3);
        assert_ne!(
            out, out2,
            "hash should change with different second input hash"
        );

        out2 = hasher.hash_node(0, &hash2, &hash1);
        assert_ne!(
            out, out2,
            "hash should change when swapping order of inputs"
        );
    }
}
