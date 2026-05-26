//! SHA-256 of the fully-assembled system prompt — the M1 prompt-determinism
//! fingerprint (issue #110).
//!
//! The M1 exit criteria require that the assembled system prompt is
//! byte-for-byte identical across a full-stack restart with matching config
//! and hydrated state, so "silent prompt drift between runs" becomes a visible
//! hash mismatch in `/api/health` and `genie-ctl status` instead of an
//! undetected behavior change.
//!
//! This is intentionally a *real* SHA-256, distinct from the operational
//! FNV-1a fingerprint in [`crate::runtime_contract`]: the README M1 checklist
//! and the dashboard speak in terms of a "system prompt SHA", so we publish a
//! genuine SHA-256 hex digest. It is implemented in pure Rust to honor the
//! crate's no-crypto-dependency policy (see `runtime_contract`); the digest is
//! a determinism fingerprint for operators, not a security primitive.
//!
//! Correctness is locked down with the NIST/FIPS-180 known-answer vectors in
//! the test module below.

const INITIAL_STATE: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

const ROUND_CONSTANTS: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Lowercase 64-character SHA-256 hex digest of `data`.
///
/// Used for the system-prompt determinism fingerprint; see the module docs.
// The `a`..`h` working variables are the canonical SHA-256 register names from
// FIPS-180; keeping them mirrors the spec and the reference test vectors.
#[allow(clippy::many_single_char_names)]
pub fn sha256_hex(data: &str) -> String {
    let mut state = INITIAL_STATE;
    let mut message = data.as_bytes().to_vec();
    let bit_len = (message.len() as u64).wrapping_mul(8);

    // Pad: append 0x80, zero-fill until 56 mod 64, then the 64-bit big-endian
    // message length in bits.
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for block in message.chunks_exact(64) {
        let mut schedule = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            schedule[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = schedule[i - 15].rotate_right(7)
                ^ schedule[i - 15].rotate_right(18)
                ^ (schedule[i - 15] >> 3);
            let s1 = schedule[i - 2].rotate_right(17)
                ^ schedule[i - 2].rotate_right(19)
                ^ (schedule[i - 2] >> 10);
            schedule[i] = schedule[i - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(ROUND_CONSTANTS[i])
                .wrapping_add(schedule[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut hex = String::with_capacity(64);
    for word in state {
        hex.push_str(&format!("{word:08x}"));
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    // NIST/FIPS-180 known-answer vectors. These pin the implementation to a
    // genuine SHA-256 — a hand-rolled hash that drifted from the standard would
    // fail here, and the determinism guarantee in issue #110 leans on these
    // being exact.
    #[test]
    fn matches_published_sha256_vectors() {
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Two-block message (exercises the schedule across block boundaries).
        assert_eq!(
            sha256_hex("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn digest_is_64_hex_chars() {
        let digest = sha256_hex("GeniePod Home");
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn same_input_is_deterministic_and_differs_on_change() {
        assert_eq!(
            sha256_hex("identical prompt"),
            sha256_hex("identical prompt")
        );
        assert_ne!(sha256_hex("prompt one"), sha256_hex("prompt two"));
    }
}
