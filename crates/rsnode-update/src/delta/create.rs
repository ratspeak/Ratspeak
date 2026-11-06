//! Patch creation for hosts. Matching follows bsdiff's idea without its suffix sort: exact seeds
//! from a hash chain over the base, the previous match's continuation tried first, then each match
//! stretched forward and backward while equal bytes outweigh different ones. The differences inside
//! a stretched match are mostly zero, which the per-block LZSS packs to a few bits.

use alloc::vec;
use alloc::vec::Vec;

use super::{
    DELTA_BLOCK_OUT, DELTA_MAGIC, DELTA_VERSION, LZSS_INDEX_BITS, LZSS_LENGTH_BITS, LZSS_MAX_MATCH,
    LZSS_MIN_MATCH, LZSS_WINDOW, OP_ADD, OP_INSERT,
};

const SEED: usize = 8;
const HASH_BITS: u32 = 22;
const CHAIN_DEPTH: usize = 48;
/// An exact seed shorter than this is not worth an operation.
const SEED_MIN: usize = 12;
/// Stop stretching once differences lead equal bytes by this much.
const STRETCH_SLACK: i64 = 48;
const LZSS_CHAIN_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Op {
    /// Target bytes `target_at..+len` as base bytes from `base_at` plus differences.
    Add {
        base_at: u32,
        target_at: u32,
        len: u32,
    },
    /// Target bytes `target_at..+len` as they are.
    Insert { target_at: u32, len: u32 },
}

/// Build a patch that reconstructs `target` from the exact `base` bytes.
/// Use firmware-sized inputs whose offsets, lengths and rounded block boundaries fit `u32`;
/// these limits are not checked before internal casts. Allocates a 16 MiB hash table plus
/// per-base-byte links and operation/output buffers. The patch may be larger than the target.
/// It carries no base hash or signature; the transfer envelope must bind the base and signed image.
pub fn create(base: &[u8], target: &[u8]) -> Vec<u8> {
    let ops = split_at_blocks(&find_ops(base, target));
    let mut patch = DELTA_MAGIC.to_vec();
    patch.extend_from_slice(&[DELTA_VERSION, 0, 0, 0]);
    patch.extend_from_slice(&(target.len() as u32).to_le_bytes());
    let block_out = DELTA_BLOCK_OUT as usize;
    let mut start = 0;
    while start < ops.len() {
        let block = target_at(&ops[start]) as usize / block_out;
        let end = ops[start..]
            .iter()
            .position(|op| target_at(op) as usize / block_out != block)
            .map_or(ops.len(), |index| start + index);
        let (base_offset, stream) = encode_ops(base, target, &ops[start..end]);
        let packed = lzss(&stream);
        patch.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        patch.extend_from_slice(&base_offset.to_le_bytes());
        patch.extend_from_slice(&packed);
        start = end;
    }
    patch
}

fn target_at(op: &Op) -> u32 {
    match *op {
        Op::Add { target_at, .. } | Op::Insert { target_at, .. } => target_at,
    }
}

fn seed_hash(bytes: &[u8]) -> usize {
    let value = u64::from_le_bytes(bytes[..SEED].try_into().expect("seed"));
    (value.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - HASH_BITS)) as usize
}

fn exact_len(base: &[u8], target: &[u8], base_at: usize, target_at: usize) -> usize {
    base[base_at..]
        .iter()
        .zip(&target[target_at..])
        .take_while(|(left, right)| left == right)
        .count()
}

/// How far a match at (`base_at`, `target_at`) stretches forward with differences in it.
fn stretch_forward(base: &[u8], target: &[u8], base_at: usize, target_at: usize) -> usize {
    let (mut score, mut best, mut best_len) = (0i64, 0i64, 0usize);
    let limit = (base.len() - base_at).min(target.len() - target_at);
    for index in 0..limit {
        score += if base[base_at + index] == target[target_at + index] {
            1
        } else {
            -1
        };
        if score > best {
            best = score;
            best_len = index + 1;
        } else if best - score > STRETCH_SLACK {
            break;
        }
    }
    best_len
}

/// How far back from (`base_at`, `target_at`) a match takes over up to `room` literal bytes.
fn stretch_backward(
    base: &[u8],
    target: &[u8],
    base_at: usize,
    target_at: usize,
    room: usize,
) -> usize {
    let (mut score, mut best, mut best_len) = (0i64, 0i64, 0usize);
    for index in 1..=room.min(base_at) {
        score += if base[base_at - index] == target[target_at - index] {
            1
        } else {
            -1
        };
        if score > best {
            best = score;
            best_len = index;
        } else if best - score > STRETCH_SLACK {
            break;
        }
    }
    best_len
}

fn find_ops(base: &[u8], target: &[u8]) -> Vec<Op> {
    let mut head = vec![u32::MAX; 1 << HASH_BITS];
    let mut prev = vec![u32::MAX; base.len()];
    for position in 0..base.len().saturating_sub(SEED - 1) {
        let hash = seed_hash(&base[position..]);
        prev[position] = head[hash];
        head[hash] = position as u32;
    }
    let mut ops = Vec::new();
    let (mut target_at, mut literal_from) = (0usize, 0usize);
    let mut continuation: Option<usize> = None;
    while target_at + SEED <= target.len() {
        let mut best: Option<(usize, usize)> = None;
        // The old code usually goes on where the last match left it.
        if let Some(base_at) = continuation.filter(|base_at| *base_at < base.len()) {
            let len = exact_len(base, target, base_at, target_at);
            if len >= SEED {
                best = Some((base_at, len));
            }
        }
        let mut candidate = head[seed_hash(&target[target_at..])];
        for _ in 0..CHAIN_DEPTH {
            if candidate == u32::MAX {
                break;
            }
            let base_at = candidate as usize;
            let len = exact_len(base, target, base_at, target_at);
            if len >= SEED_MIN && best.is_none_or(|(_, best_len)| len > best_len) {
                best = Some((base_at, len));
            }
            candidate = prev[base_at];
        }
        let Some((base_at, _)) = best else {
            target_at += 1;
            continue;
        };
        let back = stretch_backward(base, target, base_at, target_at, target_at - literal_from);
        let forward = stretch_forward(base, target, base_at, target_at).max(1);
        let (start_base, start_target) = (base_at - back, target_at - back);
        if start_target > literal_from {
            ops.push(Op::Insert {
                target_at: literal_from as u32,
                len: (start_target - literal_from) as u32,
            });
        }
        ops.push(Op::Add {
            base_at: start_base as u32,
            target_at: start_target as u32,
            len: (back + forward) as u32,
        });
        target_at += forward;
        literal_from = target_at;
        continuation = Some(base_at + forward);
    }
    if literal_from < target.len() {
        ops.push(Op::Insert {
            target_at: literal_from as u32,
            len: (target.len() - literal_from) as u32,
        });
    }
    ops
}

/// Cut operations at block boundaries so every block rebuilds exactly its own output.
fn split_at_blocks(ops: &[Op]) -> Vec<Op> {
    let mut split = Vec::with_capacity(ops.len());
    for &op in ops {
        let mut op = op;
        loop {
            let (start, len) = match op {
                Op::Add { target_at, len, .. } | Op::Insert { target_at, len } => (target_at, len),
            };
            let boundary = (start / DELTA_BLOCK_OUT + 1) * DELTA_BLOCK_OUT;
            if start + len <= boundary {
                split.push(op);
                break;
            }
            let head = boundary - start;
            match op {
                Op::Add {
                    base_at,
                    target_at,
                    len,
                } => {
                    split.push(Op::Add {
                        base_at,
                        target_at,
                        len: head,
                    });
                    op = Op::Add {
                        base_at: base_at + head,
                        target_at: target_at + head,
                        len: len - head,
                    };
                }
                Op::Insert { target_at, len } => {
                    split.push(Op::Insert {
                        target_at,
                        len: head,
                    });
                    op = Op::Insert {
                        target_at: target_at + head,
                        len: len - head,
                    };
                }
            }
        }
    }
    split
}

fn leb128(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// One block's operations, uncompressed, and the base cursor they start from.
fn encode_ops(base: &[u8], target: &[u8], ops: &[Op]) -> (u32, Vec<u8>) {
    let start = ops
        .iter()
        .find_map(|op| match *op {
            Op::Add { base_at, .. } => Some(base_at),
            Op::Insert { .. } => None,
        })
        .unwrap_or(0);
    let mut cursor = i64::from(start);
    let mut stream = Vec::new();
    for &op in ops {
        match op {
            Op::Add {
                base_at,
                target_at,
                len,
            } => {
                stream.push(OP_ADD);
                leb128(&mut stream, u64::from(len));
                let seek = i64::from(base_at) - cursor;
                leb128(&mut stream, ((seek << 1) ^ (seek >> 63)) as u64);
                let (base_at, target_at) = (base_at as usize, target_at as usize);
                stream
                    .extend((0..len as usize).map(|index| {
                        target[target_at + index].wrapping_sub(base[base_at + index])
                    }));
                cursor = (base_at + len as usize) as i64;
            }
            Op::Insert { target_at, len } => {
                stream.push(OP_INSERT);
                leb128(&mut stream, u64::from(len));
                stream.extend_from_slice(&target[target_at as usize..(target_at + len) as usize]);
            }
        }
    }
    (start, stream)
}

struct BitWriter {
    bytes: Vec<u8>,
    acc: u64,
    count: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, count: u32) {
        self.acc = (self.acc << count) | u64::from(value);
        self.count += count;
        while self.count >= 8 {
            self.count -= 8;
            self.bytes.push((self.acc >> self.count) as u8);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            let pad = 8 - self.count;
            self.put(0, pad);
        }
        self.bytes
    }
}

/// LZSS over one block, greedy with one step of lookahead, matches within the 1 KiB window.
fn lzss(input: &[u8]) -> Vec<u8> {
    const HASH: usize = 1 << 14;
    let mut head = vec![usize::MAX; HASH];
    let mut prev = vec![usize::MAX; input.len()];
    let hash = |at: usize| {
        let value = u32::from(input[at])
            | (u32::from(input[at + 1]) << 8)
            | (u32::from(input[at + 2]) << 16);
        (value.wrapping_mul(0x9E37_79B1) >> 18) as usize % HASH
    };
    let insert = |at: usize, head: &mut [usize], prev: &mut [usize]| {
        if at + LZSS_MIN_MATCH <= input.len() {
            let key = hash(at);
            prev[at] = head[key];
            head[key] = at;
        }
    };
    let longest = |at: usize, head: &[usize], prev: &[usize]| -> (usize, usize) {
        if at + LZSS_MIN_MATCH > input.len() {
            return (0, 0);
        }
        let (mut best_len, mut best_distance) = (0, 0);
        let mut candidate = head[hash(at)];
        for _ in 0..LZSS_CHAIN_DEPTH {
            if candidate == usize::MAX || at - candidate > LZSS_WINDOW {
                break;
            }
            let len = input[candidate..]
                .iter()
                .zip(&input[at..])
                .take(LZSS_MAX_MATCH)
                .take_while(|(left, right)| left == right)
                .count();
            if len > best_len {
                best_len = len;
                best_distance = at - candidate;
                if len == LZSS_MAX_MATCH {
                    break;
                }
            }
            candidate = prev[candidate];
        }
        (best_len, best_distance)
    };
    let mut bits = BitWriter {
        bytes: Vec::with_capacity(input.len() / 2),
        acc: 0,
        count: 0,
    };
    let mut at = 0;
    let lookahead = LZSS_MIN_MATCH..LZSS_MAX_MATCH;
    while at < input.len() {
        let (len, distance) = longest(at, &head, &prev);
        let lazier = lookahead.contains(&len) && {
            insert(at, &mut head, &mut prev);
            let (next, _) = longest(at + 1, &head, &prev);
            next > len + 1
        };
        if len >= LZSS_MIN_MATCH && !lazier {
            bits.put(0, 1);
            bits.put((distance - 1) as u32, LZSS_INDEX_BITS);
            bits.put((len - LZSS_MIN_MATCH) as u32, LZSS_LENGTH_BITS);
            // The first position may already be in the chain from the lookahead.
            if !lookahead.contains(&len) {
                insert(at, &mut head, &mut prev);
            }
            for skipped in at + 1..at + len {
                insert(skipped, &mut head, &mut prev);
            }
            at += len;
        } else {
            bits.put(1, 1);
            bits.put(u32::from(input[at]), 8);
            if len < LZSS_MIN_MATCH {
                insert(at, &mut head, &mut prev);
            }
            at += 1;
        }
    }
    bits.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta::{DeltaApplier, DeltaIo};

    struct VecIo<'a> {
        base: &'a [u8],
        out: Vec<u8>,
    }

    impl DeltaIo for VecIo<'_> {
        type Error = ();

        fn read_base(&mut self, offset: u32, buffer: &mut [u8]) -> Result<(), ()> {
            buffer.copy_from_slice(&self.base[offset as usize..offset as usize + buffer.len()]);
            Ok(())
        }

        fn write(&mut self, bytes: &[u8]) -> Result<(), ()> {
            self.out.extend_from_slice(bytes);
            Ok(())
        }
    }

    pub(crate) fn apply(base: &[u8], patch: &[u8], target_len: usize, step: usize) -> Vec<u8> {
        let mut applier = DeltaApplier::new(target_len as u32, base.len() as u32);
        let mut io = VecIo {
            base,
            out: Vec::new(),
        };
        for piece in patch.chunks(step) {
            applier.push(piece, &mut io).unwrap();
        }
        assert!(applier.complete());
        io.out
    }

    /// Code-like bytes: a deterministic stream with structure and repeats.
    fn firmware_like(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed;
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            match (state >> 60) & 3 {
                0 => out.extend_from_slice(&[0; 16]),
                1 => out.extend_from_slice(&(state as u32).to_le_bytes()),
                _ => out.extend_from_slice(&state.to_le_bytes()),
            }
        }
        out.truncate(len);
        out
    }

    #[test]
    fn patches_rebuild_the_target_exactly_in_any_chunking() {
        let base = firmware_like(300_000, 1);
        let mut target = base.clone();
        // A release's worth of change: inserted code, moved pointers, a new tail.
        target.splice(50_000..50_000, firmware_like(3_000, 2));
        for at in (100_000..200_000).step_by(997) {
            target[at] = target[at].wrapping_add(4);
        }
        target.extend(firmware_like(10_000, 3));
        let patch = create(&base, &target);
        for step in [1, 420, patch.len()] {
            assert_eq!(apply(&base, &patch, target.len(), step), target);
        }
        assert!(
            patch.len() < target.len() / 10,
            "{} bytes for {}",
            patch.len(),
            target.len()
        );
    }

    #[test]
    fn unrelated_and_tiny_images_still_round_trip() {
        let base = firmware_like(5_000, 7);
        let target = firmware_like(70_000, 8);
        assert_eq!(
            apply(&base, &create(&base, &target), target.len(), 333),
            target
        );
        let target = b"short".to_vec();
        assert_eq!(
            apply(&base, &create(&base, &target), target.len(), 1),
            target
        );
        let empty_base: Vec<u8> = Vec::new();
        let target = firmware_like(1_000, 9);
        assert_eq!(
            apply(&empty_base, &create(&empty_base, &target), target.len(), 64),
            target
        );
    }

    #[test]
    fn small_patch_matches_the_released_encoder() {
        let base: Vec<u8> = (0..200u32).map(|index| (index * 7 % 251) as u8).collect();
        let mut target = base.clone();
        target[40] ^= 0x55;
        target[41] = target[41].wrapping_add(3);
        target.splice(100..100, b"rsNode!".iter().copied());
        target.extend_from_slice(b"tail");
        let expected = "5253445001000000d3000000210000000000000080d9200002595c0e0000368141ee573a75bec9659086fa0007302825d2c369b600";
        let expected: Vec<u8> = (0..expected.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&expected[at..at + 2], 16).unwrap())
            .collect();
        assert_eq!(create(&base, &target), expected);
    }
}
