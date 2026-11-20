//! The digests of the checksum utilities and git: MD5 (RFC 1321) and BLAKE2b-512
//! (RFC 7693), written here from the specifications, and SHA-1 and the SHA-2 family (FIPS 180-4) from nanocode-digest.
//! Streaming: [`Digest::update`] any number of times, then [`Digest::finish`]. The tests
//! check the specifications' vectors.

use nanocode_digest::{Sha1, Sha224, Sha256, Sha384, Sha512, Sha512_224, Sha512_256};

/// A digest algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algo {
    Md5,
    Sha1,
    Sha224,
    Sha256,
    Sha384,
    Sha512,
    Sha512_224,
    Sha512_256,
    Blake2b,
}

impl Algo {
    /// The name in BSD-style lines (`SHA256 (file) = ...`), as GNU `--tag` writes it.
    pub fn tag(self) -> &'static str {
        match self {
            Algo::Md5 => "MD5",
            Algo::Sha1 => "SHA1",
            Algo::Sha224 => "SHA224",
            Algo::Sha256 => "SHA256",
            Algo::Sha384 => "SHA384",
            Algo::Sha512 => "SHA512",
            Algo::Sha512_224 => "SHA512/224",
            Algo::Sha512_256 => "SHA512/256",
            Algo::Blake2b => "BLAKE2b",
        }
    }

    /// The digest's length in bytes.
    pub fn len(self) -> usize {
        match self {
            Algo::Md5 => MD5_OUTPUT_BYTES,
            Algo::Sha1 => Sha1::OUTPUT_BYTES,
            Algo::Sha224 => Sha224::OUTPUT_BYTES,
            Algo::Sha256 => Sha256::OUTPUT_BYTES,
            Algo::Sha384 => Sha384::OUTPUT_BYTES,
            Algo::Sha512 => Sha512::OUTPUT_BYTES,
            Algo::Sha512_224 => Sha512_224::OUTPUT_BYTES,
            Algo::Sha512_256 => Sha512_256::OUTPUT_BYTES,
            Algo::Blake2b => BLAKE2B_OUTPUT_BYTES,
        }
    }
}

pub struct Digest(State);

enum State {
    Md5(Md5),
    Sha1(Sha1),
    Sha224(Sha224),
    Sha256(Sha256),
    Sha384(Sha384),
    Sha512(Sha512),
    Sha512_224(Sha512_224),
    Sha512_256(Sha512_256),
    Blake2b(Blake2b),
}

const MD5_BLOCK_BYTES: usize = 64;
const MD5_LENGTH_BYTES: usize = 8;
const MD5_OUTPUT_BYTES: usize = 16;
const PADDING_MARKER: u8 = 0x80;

const MD5_INITIAL: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];

const MD5_SHIFTS: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

const MD5_SINES: [u32; 64] = [
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

impl Digest {
    pub fn new(algo: Algo) -> Digest {
        Digest(match algo {
            Algo::Md5 => State::Md5(Md5::new()),
            Algo::Sha1 => State::Sha1(Sha1::new()),
            Algo::Sha224 => State::Sha224(Sha224::new()),
            Algo::Sha256 => State::Sha256(Sha256::new()),
            Algo::Sha384 => State::Sha384(Sha384::new()),
            Algo::Sha512 => State::Sha512(Sha512::new()),
            Algo::Sha512_224 => State::Sha512_224(Sha512_224::new()),
            Algo::Sha512_256 => State::Sha512_256(Sha512_256::new()),
            Algo::Blake2b => State::Blake2b(Blake2b::new()),
        })
    }

    pub fn update(&mut self, data: &[u8]) {
        match &mut self.0 {
            State::Md5(hasher) => hasher.update(data),
            State::Sha1(hasher) => hasher.update(data),
            State::Sha224(hasher) => hasher.update(data),
            State::Sha256(hasher) => hasher.update(data),
            State::Sha384(hasher) => hasher.update(data),
            State::Sha512(hasher) => hasher.update(data),
            State::Sha512_224(hasher) => hasher.update(data),
            State::Sha512_256(hasher) => hasher.update(data),
            State::Blake2b(hasher) => hasher.update(data),
        }
    }

    pub fn finish(self) -> Vec<u8> {
        match self.0 {
            State::Md5(hasher) => hasher.finish().to_vec(),
            State::Sha1(hasher) => hasher.finalize().to_vec(),
            State::Sha224(hasher) => hasher.finalize().to_vec(),
            State::Sha256(hasher) => hasher.finalize().to_vec(),
            State::Sha384(hasher) => hasher.finalize().to_vec(),
            State::Sha512(hasher) => hasher.finalize().to_vec(),
            State::Sha512_224(hasher) => hasher.finalize().to_vec(),
            State::Sha512_256(hasher) => hasher.finalize().to_vec(),
            State::Blake2b(hasher) => hasher.finish().to_vec(),
        }
    }
}

const BLAKE2B_BLOCK_BYTES: usize = 128;
const BLAKE2B_OUTPUT_BYTES: usize = 64;
const BLAKE2B_IV: [u64; 8] = [
    0x6a09_e667_f3bc_c908,
    0xbb67_ae85_84ca_a73b,
    0x3c6e_f372_fe94_f82b,
    0xa54f_f53a_5f1d_36f1,
    0x510e_527f_ade6_82d1,
    0x9b05_688c_2b3e_6c1f,
    0x1f83_d9ab_fb41_bd6b,
    0x5be0_cd19_137e_2179,
];
const BLAKE2B_SIGMA: [[usize; 16]; 12] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
];

/// BLAKE2b with a 64-byte digest and no key, as b2sum computes by default.
struct Blake2b {
    h: [u64; 8],
    block: [u8; BLAKE2B_BLOCK_BYTES],
    block_filled: usize,
    length_bytes: u128,
}

impl Blake2b {
    fn new() -> Blake2b {
        let mut h = BLAKE2B_IV;
        // Parameter block: digest length 64, no key, fanout 1, depth 1.
        h[0] ^= 0x0101_0000 ^ BLAKE2B_OUTPUT_BYTES as u64;
        Blake2b { h, block: [0; BLAKE2B_BLOCK_BYTES], block_filled: 0, length_bytes: 0 }
    }

    fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            // The last block is compressed in finish, flagged final, so a full block
            // waits until more data arrives.
            if self.block_filled == BLAKE2B_BLOCK_BYTES {
                self.length_bytes += BLAKE2B_BLOCK_BYTES as u128;
                let block = self.block;
                blake2b_compress(&mut self.h, &block, self.length_bytes, false);
                self.block_filled = 0;
            }
            let take = (BLAKE2B_BLOCK_BYTES - self.block_filled).min(data.len());
            self.block[self.block_filled..self.block_filled + take].copy_from_slice(&data[..take]);
            self.block_filled += take;
            data = &data[take..];
        }
    }

    fn finish(mut self) -> [u8; BLAKE2B_OUTPUT_BYTES] {
        self.length_bytes += self.block_filled as u128;
        self.block[self.block_filled..].fill(0);
        let block = self.block;
        blake2b_compress(&mut self.h, &block, self.length_bytes, true);
        let mut out = [0; BLAKE2B_OUTPUT_BYTES];
        for (chunk, word) in out.as_chunks_mut::<8>().0.iter_mut().zip(self.h) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        out
    }
}

fn blake2b_compress(h: &mut [u64; 8], block: &[u8; BLAKE2B_BLOCK_BYTES], t: u128, last: bool) {
    let mut m = [0u64; 16];
    for (word, chunk) in m.iter_mut().zip(block.as_chunks::<8>().0) {
        *word = u64::from_le_bytes(*chunk);
    }
    let mut v = [0u64; 16];
    v[..8].copy_from_slice(h);
    v[8..].copy_from_slice(&BLAKE2B_IV);
    v[12] ^= t as u64;
    v[13] ^= (t >> 64) as u64;
    if last {
        v[14] = !v[14];
    }
    let g = |v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64| {
        v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
        v[d] = (v[d] ^ v[a]).rotate_right(32);
        v[c] = v[c].wrapping_add(v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(24);
        v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
        v[d] = (v[d] ^ v[a]).rotate_right(16);
        v[c] = v[c].wrapping_add(v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(63);
    };
    for s in &BLAKE2B_SIGMA {
        g(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
        g(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
        g(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
        g(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
        g(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
        g(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
        g(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
        g(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
    }
    for i in 0..8 {
        h[i] ^= v[i] ^ v[i + 8];
    }
}

struct Md5 {
    words: [u32; 4],
    block: [u8; MD5_BLOCK_BYTES],
    block_filled: usize,
    length_bytes: u64,
}

impl Md5 {
    fn new() -> Md5 {
        Md5 {
            words: MD5_INITIAL,
            block: [0; MD5_BLOCK_BYTES],
            block_filled: 0,
            length_bytes: 0,
        }
    }

    fn update(&mut self, data: &[u8]) {
        self.length_bytes = self.length_bytes.wrapping_add(data.len() as u64);
        let mut rest = data;
        if self.block_filled > 0 {
            let take = (MD5_BLOCK_BYTES - self.block_filled).min(rest.len());
            self.block[self.block_filled..self.block_filled + take].copy_from_slice(&rest[..take]);
            self.block_filled += take;
            rest = &rest[take..];
            if self.block_filled < MD5_BLOCK_BYTES {
                return;
            }
            let block = self.block;
            md5_compress(&mut self.words, &block);
            self.block_filled = 0;
        }
        let (blocks, tail) = rest.as_chunks::<MD5_BLOCK_BYTES>();
        for block in blocks {
            md5_compress(&mut self.words, block);
        }
        self.block[..tail.len()].copy_from_slice(tail);
        self.block_filled = tail.len();
    }

    fn finish(mut self) -> [u8; MD5_OUTPUT_BYTES] {
        let mut tail = [0u8; 2 * MD5_BLOCK_BYTES];
        let filled = self.block_filled;
        tail[..filled].copy_from_slice(&self.block[..filled]);
        tail[filled] = PADDING_MARKER;
        let total = if filled + 1 + MD5_LENGTH_BYTES <= MD5_BLOCK_BYTES {
            MD5_BLOCK_BYTES
        } else {
            2 * MD5_BLOCK_BYTES
        };
        tail[total - MD5_LENGTH_BYTES..total]
            .copy_from_slice(&self.length_bytes.wrapping_mul(8).to_le_bytes());
        for block in tail[..total].as_chunks::<MD5_BLOCK_BYTES>().0 {
            md5_compress(&mut self.words, block);
        }
        let mut out = [0u8; MD5_OUTPUT_BYTES];
        for (bytes, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.words) {
            *bytes = word.to_le_bytes();
        }
        out
    }
}

fn md5_compress(state: &mut [u32; 4], block: &[u8]) {
    assert_eq!(block.len(), MD5_BLOCK_BYTES);
    let mut message = [0u32; 16];
    for (word, bytes) in message.iter_mut().zip(block.as_chunks::<4>().0) {
        *word = u32::from_le_bytes(*bytes);
    }
    let [mut a, mut b, mut c, mut d] = *state;
    for round in 0..64 {
        let (mixed, index) = match round / 16 {
            0 => ((b & c) | (!b & d), round),
            1 => ((d & b) | (!d & c), (5 * round + 1) % 16),
            2 => (b ^ c ^ d, (3 * round + 5) % 16),
            _ => (c ^ (b | !d), (7 * round) % 16),
        };
        let sum = a
            .wrapping_add(mixed)
            .wrapping_add(MD5_SINES[round])
            .wrapping_add(message[index]);
        a = d;
        d = c;
        c = b;
        b = b.wrapping_add(sum.rotate_left(MD5_SHIFTS[round]));
    }
    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
}

/// The digest of `data`.
pub fn digest(algo: Algo, data: &[u8]) -> Vec<u8> {
    let mut d = Digest::new(algo);
    d.update(data);
    d.finish()
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[usize::from(b >> 4)] as char);
        s.push(DIGITS[usize::from(b & 15)] as char);
    }
    s
}
