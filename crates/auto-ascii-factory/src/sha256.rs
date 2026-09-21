//! Minimal SHA-256 (FIPS 180-4) — the eval cache key hash (M2 item B:
//! assets cached by `(input sha, params sha)`), the determinism-guard
//! fingerprint and the provenance hash `auto-ascii import` records.
//! Hand-rolled rather than a new dependency: the factory needs exactly
//! "hash these bytes", nothing keyed.
//!
//! INCREMENTAL (M8 review): the state is the eight working words plus a
//! tail of at most 63 bytes, full blocks are compressed straight out of
//! the caller's slice, and [`sha256_file`] streams 64 KiB at a time. A
//! 4 GB source used to cost 8 GB of resident memory here — the file read
//! whole, then copied again for the padding — for a digest that never
//! needed more than one block in hand.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// The initial state (FIPS 180-4 §5.3.3).
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
    0x5be0cd19,
];

/// How much of a file [`sha256_file`] holds at once.
const CHUNK: usize = 64 * 1024;

/// An in-progress SHA-256: the eight working words, the bytes that have
/// not filled a block yet, and how many bytes have been absorbed in total.
/// 104 bytes of state, whatever the size of the message.
#[derive(Clone, Debug)]
pub struct Sha256 {
    h: [u32; 8],
    /// Bytes waiting for a full block — only `tail_len` of these matter.
    tail: [u8; 64],
    tail_len: usize,
    /// Message length in BYTES (the padding needs it in bits).
    total: u64,
}

impl Default for Sha256 {
    fn default() -> Sha256 {
        Sha256::new()
    }
}

impl Sha256 {
    /// A hasher over the empty message.
    pub fn new() -> Sha256 {
        Sha256 { h: H0, tail: [0; 64], tail_len: 0, total: 0 }
    }

    /// Absorb `data`. Any number of calls, any sizes: the digest depends
    /// only on the concatenation.
    pub fn update(&mut self, data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        self.absorb(data);
    }

    /// Pad and produce the digest.
    pub fn finish(mut self) -> [u8; 32] {
        // Padding (§5.1.1): 0x80, zeros up to 56 mod 64, then the length
        // in bits as a big-endian u64.
        let bits = self.total.wrapping_mul(8);
        let mut pad = [0u8; 64];
        pad[0] = 0x80;
        let zeros = if self.tail_len < 56 { 56 - self.tail_len } else { 120 - self.tail_len };
        self.absorb(&pad[..zeros]);
        self.absorb(&bits.to_be_bytes());
        debug_assert_eq!(self.tail_len, 0, "padding must land on a block boundary");

        let mut out = [0u8; 32];
        for (chunk, word) in out.chunks_exact_mut(4).zip(self.h) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    /// Compress everything `data` completes, keeping the remainder. Does
    /// NOT count the bytes — [`finish`](Sha256::finish) absorbs padding
    /// this way, and padding is not message.
    fn absorb(&mut self, mut data: &[u8]) {
        // Top up a partial tail first; a full one is a block.
        if self.tail_len > 0 {
            let take = (64 - self.tail_len).min(data.len());
            self.tail[self.tail_len..self.tail_len + take].copy_from_slice(&data[..take]);
            self.tail_len += take;
            data = &data[take..];
            if self.tail_len == 64 {
                let block = self.tail;
                compress(&mut self.h, &block);
                self.tail_len = 0;
            }
        }
        // Then every whole block straight from the caller's slice: this is
        // the path a big file takes, and it copies nothing.
        let mut blocks = data.chunks_exact(64);
        for block in &mut blocks {
            compress(&mut self.h, block.try_into().expect("chunks_exact(64)"));
        }
        // Guarded: reaching here with a PARTIAL tail means `data` was
        // swallowed by the top-up above, and a bare assignment would zero
        // the length that top-up just set.
        let rest = blocks.remainder();
        if !rest.is_empty() {
            self.tail[self.tail_len..self.tail_len + rest.len()].copy_from_slice(rest);
            self.tail_len += rest.len();
        }
    }
}

/// One 64-byte block into the state (FIPS 180-4 §6.2.2).
fn compress(h: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (i, word) in block.chunks_exact(4).enumerate() {
        w[i] = u32::from_be_bytes(word.try_into().expect("chunks_exact(4)"));
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (s, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
        *s = s.wrapping_add(v);
    }
}

/// SHA-256 digest of `data`.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finish()
}

/// Lowercase hex digest.
pub fn sha256_hex(data: &[u8]) -> String {
    hex(sha256(data))
}

/// Hex digest of a file's contents, read [`CHUNK`] bytes at a time — a
/// 4 GB video costs 64 KiB of memory here, not 4 GB.
pub fn sha256_file(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            return Ok(hex(hasher.finish()));
        }
        hasher.update(&buf[..read]);
    }
}

/// A digest as lowercase hex.
fn hex(digest: [u8; 32]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 180-4 / NIST CAVP reference vectors.
    #[test]
    fn nist_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // Padding boundary cases: 55/56/64 bytes exercise both pad branches.
        assert_eq!(
            sha256_hex(&[0x61u8; 55]),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            sha256_hex(&[0x61u8; 56]),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            sha256_hex(&[0x61u8; 64]),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    /// Deterministic filler with no repeating block pattern, so a hasher
    /// that dropped or reordered a chunk could not still agree.
    fn filler(len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut x: u32 = 0x1234_5678;
        while out.len() < len {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            out.extend_from_slice(&x.to_le_bytes());
        }
        out.truncate(len);
        out
    }

    /// The digest depends on the bytes, not on how they arrived: feeding
    /// them in awkward pieces (never a multiple of the 64-byte block, and
    /// crossing it repeatedly) must match the one-shot hash.
    #[test]
    fn incremental_matches_one_shot() {
        let data = filler(1000);
        for piece in [1usize, 7, 63, 64, 65, 100] {
            let mut hasher = Sha256::new();
            for chunk in data.chunks(piece) {
                hasher.update(chunk);
            }
            assert_eq!(hex(hasher.finish()), sha256_hex(&data), "in pieces of {piece}");
        }
        // The empty message through the incremental path too.
        assert_eq!(hex(Sha256::new().finish()), sha256_hex(b""));
    }

    /// `sha256_file` streams in 64 KiB chunks, so the interesting case is
    /// a file of several chunks plus a tail that is not block-aligned.
    #[test]
    fn a_streamed_file_hashes_like_its_bytes() {
        let data = filler(2 * CHUNK + 37);
        let path = std::env::temp_dir()
            .join(format!("auto-ascii-sha256-{}.bin", std::process::id()));
        std::fs::write(&path, &data).expect("write the fixture");
        let streamed = sha256_file(&path).expect("hash the fixture");
        let _ = std::fs::remove_file(&path);
        assert_eq!(streamed, sha256_hex(&data));
    }
}
