//! Streaming delta patches against a fixed base image.
//!
//! Blocks decode independently and produce up to [`DELTA_BLOCK_OUT`] bytes.
//! The caller binds the patch and base to the intended transfer, authenticates
//! the target manifest, and verifies the reconstructed image's digest before
//! activation. This decoder checks patch structure, not signatures or hashes.
//! Persist checkpoints only after the corresponding output is durable.
//!
//! Layout, little-endian:
//! - header: `"RSDP" version(1) zero(3) target_len(4)`;
//! - each block: `packed_len(4) base_offset(4)`, then `packed_len` bytes of LZSS holding the
//!   block's operations, which produce exactly its output;
//! - operations: `0x01 len seek` then `len` difference bytes, each added (mod 256) to the base
//!   byte at the cursor, which first moves by `seek` (bsdiff's diff: moved code becomes runs of
//!   zeros); `0x02 len` then `len` new bytes. `len` is a LEB128 varint, `seek` a zigzag one.
//!
//! LZSS: per item one flag bit, most significant first. 1: eight bits of a literal byte follow;
//! 0: [`LZSS_INDEX_BITS`] bits of distance − 1, then [`LZSS_LENGTH_BITS`] bits of length −
//! [`LZSS_MIN_MATCH`]. Each block starts with a zeroed 1,024-byte window. The
//! applier also owns 256-byte output and base-read buffers; it does not allocate.
//! The decoder accepts nonminimal varints and ignores fewer than eight trailing
//! padding bits. Bound the total patch size and processing work at the caller.
//!
//! With the `alloc` feature, `create` builds patches on hosts.

pub const DELTA_MAGIC: [u8; 4] = *b"RSDP";
pub const DELTA_VERSION: u8 = 1;
pub const DELTA_HEADER_LEN: usize = 12;
pub const DELTA_BLOCK_HEADER_LEN: usize = 8;
/// Maximum output bytes per block. The final block can be shorter.
/// Durability depends on the output adapter, not on reaching this boundary.
pub const DELTA_BLOCK_OUT: u32 = 64 * 1024;
pub const LZSS_INDEX_BITS: u32 = 10;
pub const LZSS_LENGTH_BITS: u32 = 6;
pub const LZSS_MIN_MATCH: usize = 3;
pub const LZSS_WINDOW: usize = 1 << LZSS_INDEX_BITS;
pub const LZSS_MAX_MATCH: usize = (1 << LZSS_LENGTH_BITS) + LZSS_MIN_MATCH - 1;
pub const OP_ADD: u8 = 0x01;
pub const OP_INSERT: u8 = 0x02;

const STAGING: usize = 256;

/// Reads from a stable base image and accepts reconstructed output in order.
/// Implementations decide when accepted output becomes durable.
pub trait DeltaIo {
    type Error;
    /// Fill all of `buffer` from the same base image at `offset`, or return an error.
    fn read_base(&mut self, offset: u32, buffer: &mut [u8]) -> Result<(), Self::Error>;
    /// Accept all bytes as the next output, or return an error.
    /// The applier has no separate flush or durability operation.
    fn write(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeltaError<E> {
    /// Not a patch this applier reads, or one that does not fit its own header.
    Malformed,
    /// An operation reached outside the base image.
    BaseRange,
    Base(E),
    Sink(E),
}

/// Offsets after a decoded block has been accepted by the output adapter.
///
/// Persist only after that output is durable, together with the exact patch,
/// base, and target identity. Public fields and [`DeltaApplier::resume`] do not
/// validate a checkpoint supplied by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Checkpoint {
    pub patch_offset: u32,
    pub output: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    Header { have: u8 },
    BlockHeader { have: u8 },
    Body { packed_left: u32, block_end: u32 },
    Done,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Op {
    Tag,
    Len { add: bool, value: u32, shift: u32 },
    Seek { len: u32, value: u32, shift: u32 },
    Add { left: u32 },
    Insert { left: u32 },
}

pub struct DeltaApplier {
    target_len: u32,
    base_len: u32,
    patch_offset: u32,
    output: u32,
    stage: Stage,
    header: [u8; DELTA_HEADER_LEN],
    window: [u8; LZSS_WINDOW],
    window_pos: usize,
    bits: u32,
    bit_count: u32,
    op: Op,
    cursor: u32,
    out: [u8; STAGING],
    out_len: usize,
    cache: [u8; STAGING],
    cache_start: u32,
    cache_len: usize,
}

impl DeltaApplier {
    /// Start at the patch header with an authenticated target length and exact base length.
    /// The constructor does not check either length; a zero target fails during parsing.
    /// Require `target_len < u32::MAX`; at the maximum value, an oversized operation can
    /// pass the saturated length check and overflow the output counter.
    pub const fn new(target_len: u32, base_len: u32) -> Self {
        Self {
            target_len,
            base_len,
            patch_offset: 0,
            output: 0,
            stage: Stage::Header { have: 0 },
            header: [0; DELTA_HEADER_LEN],
            window: [0; LZSS_WINDOW],
            window_pos: 0,
            bits: 0,
            bit_count: 0,
            op: Op::Tag,
            cursor: 0,
            out: [0; STAGING],
            out_len: 0,
            cache: [0; STAGING],
            cache_start: 0,
            cache_len: 0,
        }
    }

    /// Resume after a durable block of the exact same patch, base, and target.
    /// Require `target_len < u32::MAX`, as for [`Self::new`].
    ///
    /// The caller must validate both offsets and retain the matching output
    /// prefix. This skips the patch header and neither reads nor hashes prior
    /// output. Any `output >= target_len` starts in the completed state, so do
    /// not pass an unchecked checkpoint or an output length beyond the target.
    pub fn resume(target_len: u32, base_len: u32, at: Checkpoint) -> Self {
        let mut applier = Self::new(target_len, base_len);
        applier.patch_offset = at.patch_offset;
        applier.output = at.output;
        applier.stage = if at.output >= target_len {
            Stage::Done
        } else {
            Stage::BlockHeader { have: 0 }
        };
        applier
    }

    /// Bytes consumed, including a byte that caused an error; wraps at `u32::MAX`.
    /// Keep the complete patch within the `u32` offset range.
    pub fn patch_offset(&self) -> u32 {
        self.patch_offset
    }

    /// Bytes decoded, including buffered output and bytes preceding a sink error.
    /// This is not a durable output count.
    pub fn output(&self) -> u32 {
        self.output
    }

    /// Whether the decoder is in its completed state.
    ///
    /// Normal decoding reaches this after all output is accepted. Unchecked
    /// resume state can also set it. It proves neither image integrity nor
    /// durability, and a later trailing-byte error does not clear it.
    pub fn complete(&self) -> bool {
        self.stage == Stage::Done
    }

    /// Consume patch bytes and return the last block boundary completed by this call.
    ///
    /// Errors can leave decoder state, counters, and output partly advanced;
    /// they are not latched. Discard the applier after any error. Restart from
    /// a validated durable checkpoint rather than continuing with this instance.
    /// A later error in one call also hides any earlier checkpoint from its
    /// return value, though output already accepted by `io` remains written.
    pub fn push<I: DeltaIo>(
        &mut self,
        patch: &[u8],
        io: &mut I,
    ) -> Result<Option<Checkpoint>, DeltaError<I::Error>> {
        let mut checkpoint = None;
        for &byte in patch {
            self.patch_offset = self.patch_offset.wrapping_add(1);
            match self.stage {
                Stage::Header { have } => {
                    self.header[usize::from(have)] = byte;
                    if usize::from(have) + 1 < DELTA_HEADER_LEN {
                        self.stage = Stage::Header { have: have + 1 };
                        continue;
                    }
                    let target = u32::from_le_bytes([
                        self.header[8],
                        self.header[9],
                        self.header[10],
                        self.header[11],
                    ]);
                    if self.header[..4] != DELTA_MAGIC
                        || self.header[4] != DELTA_VERSION
                        || self.header[5..8] != [0; 3]
                        || target != self.target_len
                        || target == 0
                    {
                        return Err(DeltaError::Malformed);
                    }
                    self.stage = Stage::BlockHeader { have: 0 };
                }
                Stage::BlockHeader { have } => {
                    self.header[usize::from(have)] = byte;
                    if usize::from(have) + 1 < DELTA_BLOCK_HEADER_LEN {
                        self.stage = Stage::BlockHeader { have: have + 1 };
                        continue;
                    }
                    let packed_left = read_u32(&self.header[..4]);
                    let base_offset = read_u32(&self.header[4..8]);
                    if packed_left == 0 || base_offset > self.base_len {
                        return Err(DeltaError::Malformed);
                    }
                    self.cursor = base_offset;
                    self.window = [0; LZSS_WINDOW];
                    self.window_pos = 0;
                    self.bits = 0;
                    self.bit_count = 0;
                    self.op = Op::Tag;
                    self.stage = Stage::Body {
                        packed_left,
                        block_end: self
                            .output
                            .saturating_add(DELTA_BLOCK_OUT)
                            .min(self.target_len),
                    };
                }
                Stage::Body {
                    packed_left,
                    block_end,
                } => {
                    self.bits = (self.bits << 8) | u32::from(byte);
                    self.bit_count += 8;
                    self.decode_items(block_end, io)?;
                    let packed_left = packed_left - 1;
                    if packed_left != 0 {
                        self.stage = Stage::Body {
                            packed_left,
                            block_end,
                        };
                        continue;
                    }
                    // The block's bytes end on its last item; what is left is padding.
                    if self.output != block_end || self.op != Op::Tag || self.bit_count >= 8 {
                        return Err(DeltaError::Malformed);
                    }
                    self.flush(io)?;
                    checkpoint = Some(Checkpoint {
                        patch_offset: self.patch_offset,
                        output: self.output,
                    });
                    self.stage = if self.output == self.target_len {
                        Stage::Done
                    } else {
                        Stage::BlockHeader { have: 0 }
                    };
                }
                Stage::Done => return Err(DeltaError::Malformed),
            }
        }
        Ok(checkpoint)
    }

    fn decode_items<I: DeltaIo>(
        &mut self,
        block_end: u32,
        io: &mut I,
    ) -> Result<(), DeltaError<I::Error>> {
        loop {
            if self.bit_count == 0 {
                return Ok(());
            }
            let literal = (self.bits >> (self.bit_count - 1)) & 1 == 1;
            let need = 1 + if literal {
                8
            } else {
                LZSS_INDEX_BITS + LZSS_LENGTH_BITS
            };
            if self.bit_count < need {
                return Ok(());
            }
            self.bit_count -= 1;
            if literal {
                let byte = self.take_bits(8) as u8;
                self.decoded(byte, block_end, io)?;
            } else {
                let distance = self.take_bits(LZSS_INDEX_BITS) as usize + 1;
                let length = self.take_bits(LZSS_LENGTH_BITS) as usize + LZSS_MIN_MATCH;
                for _ in 0..length {
                    let byte =
                        self.window[(self.window_pos + LZSS_WINDOW - distance) % LZSS_WINDOW];
                    self.decoded(byte, block_end, io)?;
                }
            }
        }
    }

    fn take_bits(&mut self, count: u32) -> u32 {
        self.bit_count -= count;
        (self.bits >> self.bit_count) & ((1 << count) - 1)
    }

    /// One decompressed byte of the block's operations.
    fn decoded<I: DeltaIo>(
        &mut self,
        byte: u8,
        block_end: u32,
        io: &mut I,
    ) -> Result<(), DeltaError<I::Error>> {
        self.window[self.window_pos] = byte;
        self.window_pos = (self.window_pos + 1) % LZSS_WINDOW;
        self.op = match self.op {
            Op::Tag => match byte {
                OP_ADD => Op::Len {
                    add: true,
                    value: 0,
                    shift: 0,
                },
                OP_INSERT => Op::Len {
                    add: false,
                    value: 0,
                    shift: 0,
                },
                _ => return Err(DeltaError::Malformed),
            },
            Op::Len { add, value, shift } => {
                let value = leb128(value, shift, byte)?;
                if byte & 0x80 != 0 {
                    Op::Len {
                        add,
                        value,
                        shift: shift + 7,
                    }
                } else if value == 0 || self.output.saturating_add(value) > block_end {
                    return Err(DeltaError::Malformed);
                } else if add {
                    Op::Seek {
                        len: value,
                        value: 0,
                        shift: 0,
                    }
                } else {
                    Op::Insert { left: value }
                }
            }
            Op::Seek { len, value, shift } => {
                let value = leb128(value, shift, byte)?;
                if byte & 0x80 != 0 {
                    Op::Seek {
                        len,
                        value,
                        shift: shift + 7,
                    }
                } else {
                    let seek = i64::from(value >> 1) ^ -i64::from(value & 1);
                    let cursor = i64::from(self.cursor) + seek;
                    if cursor < 0 || cursor + i64::from(len) > i64::from(self.base_len) {
                        return Err(DeltaError::BaseRange);
                    }
                    self.cursor = cursor as u32;
                    Op::Add { left: len }
                }
            }
            Op::Add { left } => {
                let base_byte = self.base_byte(io)?;
                self.cursor += 1;
                self.emit(base_byte.wrapping_add(byte), io)?;
                if left == 1 {
                    Op::Tag
                } else {
                    Op::Add { left: left - 1 }
                }
            }
            Op::Insert { left } => {
                self.emit(byte, io)?;
                if left == 1 {
                    Op::Tag
                } else {
                    Op::Insert { left: left - 1 }
                }
            }
        };
        Ok(())
    }

    fn base_byte<I: DeltaIo>(&mut self, io: &mut I) -> Result<u8, DeltaError<I::Error>> {
        let index = self.cursor.wrapping_sub(self.cache_start) as usize;
        if self.cursor < self.cache_start || index >= self.cache_len {
            let len = ((self.base_len - self.cursor) as usize).min(STAGING);
            io.read_base(self.cursor, &mut self.cache[..len])
                .map_err(DeltaError::Base)?;
            self.cache_start = self.cursor;
            self.cache_len = len;
            return Ok(self.cache[0]);
        }
        Ok(self.cache[index])
    }

    fn emit<I: DeltaIo>(&mut self, byte: u8, io: &mut I) -> Result<(), DeltaError<I::Error>> {
        self.out[self.out_len] = byte;
        self.out_len += 1;
        self.output += 1;
        if self.out_len == STAGING {
            self.flush(io)?;
        }
        Ok(())
    }

    fn flush<I: DeltaIo>(&mut self, io: &mut I) -> Result<(), DeltaError<I::Error>> {
        if self.out_len != 0 {
            io.write(&self.out[..self.out_len])
                .map_err(DeltaError::Sink)?;
            self.out_len = 0;
        }
        Ok(())
    }
}

fn leb128<E>(value: u32, shift: u32, byte: u8) -> Result<u32, DeltaError<E>> {
    if shift > 28 || (shift == 28 && byte & 0x70 != 0) {
        return Err(DeltaError::Malformed);
    }
    Ok(value | (u32::from(byte & 0x7f) << shift))
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(any(test, feature = "alloc"))]
mod create;
#[cfg(feature = "alloc")]
pub use create::create;

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    /// Bits MSB first, the way the applier reads them.
    #[derive(Default)]
    struct Bits {
        bytes: Vec<u8>,
        acc: u64,
        count: u32,
    }

    impl Bits {
        fn put(&mut self, value: u32, count: u32) {
            self.acc = (self.acc << count) | u64::from(value);
            self.count += count;
            while self.count >= 8 {
                self.count -= 8;
                self.bytes.push((self.acc >> self.count) as u8);
            }
        }

        fn literal(&mut self, byte: u8) {
            self.put(1, 1);
            self.put(u32::from(byte), 8);
        }

        fn matched(&mut self, distance: usize, length: usize) {
            self.put(0, 1);
            self.put((distance - 1) as u32, LZSS_INDEX_BITS);
            self.put((length - LZSS_MIN_MATCH) as u32, LZSS_LENGTH_BITS);
        }

        fn finish(mut self) -> Vec<u8> {
            if self.count != 0 {
                let pad = 8 - self.count;
                self.put(0, pad);
            }
            self.bytes
        }
    }

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

    fn block(base_offset: u32, packed: &[u8]) -> Vec<u8> {
        let mut out = (packed.len() as u32).to_le_bytes().to_vec();
        out.extend_from_slice(&base_offset.to_le_bytes());
        out.extend_from_slice(packed);
        out
    }

    fn header(target_len: u32) -> Vec<u8> {
        let mut out = DELTA_MAGIC.to_vec();
        out.extend_from_slice(&[DELTA_VERSION, 0, 0, 0]);
        out.extend_from_slice(&target_len.to_le_bytes());
        out
    }

    fn apply(
        patch: &[u8],
        base: &[u8],
        target_len: u32,
        step: usize,
    ) -> Result<Vec<u8>, DeltaError<()>> {
        let mut applier = DeltaApplier::new(target_len, base.len() as u32);
        let mut io = VecIo {
            base,
            out: Vec::new(),
        };
        for piece in patch.chunks(step) {
            applier.push(piece, &mut io)?;
        }
        if !applier.complete() {
            return Err(DeltaError::Malformed);
        }
        Ok(io.out)
    }

    #[test]
    fn adds_inserts_and_matches_rebuild_the_target_in_any_chunking() {
        let base: Vec<u8> = (0..64u8).collect();
        // Copy base[10..20] shifted by +1, insert "xyz", then base[40..44] unchanged.
        let mut ops = Bits::default();
        for byte in [OP_ADD, 10, 20] {
            ops.literal(byte); // len 10, seek +10 (zigzag 20)
        }
        ops.literal(1);
        ops.matched(1, 9); // nine more differences of 1, from the window
        for byte in [OP_INSERT, 3, b'x', b'y', b'z', OP_ADD, 4, 40] {
            ops.literal(byte); // seek +20 from 20 (zigzag 40)
        }
        for _ in 0..4 {
            ops.literal(0);
        }
        let mut target: Vec<u8> = (11..21).collect();
        target.extend_from_slice(b"xyz");
        target.extend_from_slice(&[40, 41, 42, 43]);
        let mut patch = header(target.len() as u32);
        patch.extend(block(0, &ops.finish()));
        for step in [1, 3, 7, patch.len()] {
            assert_eq!(
                apply(&patch, &base, target.len() as u32, step),
                Ok(target.clone())
            );
        }
    }

    #[test]
    fn blocks_are_resumable_at_their_boundaries() {
        let base = std::vec![7u8; 16];
        let len = DELTA_BLOCK_OUT + 5;
        // Two blocks of inserts: a run of 0xAB through matches, then five bytes.
        let mut first = Bits::default();
        let mut remaining = DELTA_BLOCK_OUT as usize;
        while remaining > 0 {
            let run = remaining.min(120);
            first.literal(OP_INSERT);
            first.literal(run as u8);
            first.literal(0xAB);
            let mut left = run - 1;
            while left > 0 {
                let take = left.min(LZSS_MAX_MATCH);
                if take < LZSS_MIN_MATCH {
                    (0..take).for_each(|_| first.literal(0xAB));
                } else {
                    first.matched(1, take);
                }
                left -= take;
            }
            remaining -= run;
        }
        let mut second = Bits::default();
        for byte in [OP_INSERT, 5, 1, 2, 3, 4, 5] {
            second.literal(byte);
        }
        let first = block(0, &first.finish());
        let mut patch = header(len);
        patch.extend_from_slice(&first);
        let boundary = patch.len() as u32;
        patch.extend(block(0, &second.finish()));

        let mut applier = DeltaApplier::new(len, base.len() as u32);
        let mut io = VecIo {
            base: &base,
            out: Vec::new(),
        };
        let checkpoint = applier
            .push(&patch[..boundary as usize], &mut io)
            .unwrap()
            .unwrap();
        let out = core::mem::take(&mut io.out);
        assert_eq!(
            checkpoint,
            Checkpoint {
                patch_offset: boundary,
                output: DELTA_BLOCK_OUT,
            }
        );
        // After a reboot: only the second block is sent again.
        let mut resumed = DeltaApplier::resume(len, base.len() as u32, checkpoint);
        resumed.push(&patch[boundary as usize..], &mut io).unwrap();
        let tail = core::mem::take(&mut io.out);
        assert!(resumed.complete());
        assert_eq!(tail, [1, 2, 3, 4, 5]);
        assert!(out.iter().all(|byte| *byte == 0xAB));
        assert_eq!(out.len() as u32, DELTA_BLOCK_OUT);
    }

    #[test]
    fn malformed_patches_and_base_overruns_are_refused() {
        let base = [0u8; 8];
        let mut wrong_magic = header(3);
        wrong_magic[0] = b'X';
        assert_eq!(apply(&wrong_magic, &base, 3, 1), Err(DeltaError::Malformed));
        assert_eq!(apply(&header(4), &base, 3, 1), Err(DeltaError::Malformed));
        // An add reaching past the base.
        let mut ops = Bits::default();
        for byte in [OP_ADD, 3, 12, 0, 0, 0] {
            ops.literal(byte); // seek +6, len 3 over an 8-byte base
        }
        let mut patch = header(3);
        patch.extend(block(0, &ops.finish()));
        assert_eq!(apply(&patch, &base, 3, 4), Err(DeltaError::BaseRange));
        // More output than the block holds.
        let mut ops = Bits::default();
        for byte in [OP_INSERT, 4, 1, 2, 3, 4] {
            ops.literal(byte);
        }
        let mut patch = header(3);
        patch.extend(block(0, &ops.finish()));
        assert_eq!(apply(&patch, &base, 3, 4), Err(DeltaError::Malformed));
        // A block that ends short of its output, and bytes after the end.
        let mut ops = Bits::default();
        for byte in [OP_INSERT, 2, 1, 2] {
            ops.literal(byte);
        }
        let mut patch = header(3);
        patch.extend(block(0, &ops.finish()));
        assert_eq!(apply(&patch, &base, 3, 4), Err(DeltaError::Malformed));
        let mut ops = Bits::default();
        for byte in [OP_INSERT, 3, 1, 2, 3] {
            ops.literal(byte);
        }
        let mut patch = header(3);
        patch.extend(block(0, &ops.finish()));
        assert_eq!(apply(&patch, &base, 3, 4), Ok(std::vec![1, 2, 3]));
        patch.push(0);
        assert_eq!(apply(&patch, &base, 3, 4), Err(DeltaError::Malformed));
    }
}
