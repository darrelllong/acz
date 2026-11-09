// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! The `.acz` stream format (version 12: blocked, parallel, checksummed framing).
//!
//! ```text
//! member  := header block* end trailer
//! header  := magic "ACZ\x0C"   (4 bytes; the last byte is the format version)
//!            model             (1 byte: 0, 1, 2 = order-0/1/2, 3 = PPM,
//!                               4 = context mixing; any other value is
//!                               rejected)
//!            flags             (1 byte: bit 0 = long-repeat removal; all
//!                               other bits must be 0)
//!            block-log2        (1 byte: maximum block size is 2^block-log2, 16..=30)
//!            window-log2       (1 byte: with flag bit 0, the long-repeat
//!                               window is 2^window-log2, block-log2 <
//!                               window-log2 <= 36; without it, must be 0)
//!            header-crc        (8 bytes LE: CRC-64/XZ of the 8 bytes above)
//! block   := kind:u8 raw-len:u32le packed-len:u32le payload[packed-len] block-crc:u64le
//!            raw-len    = bytes this block contributes to the output (1..=2^block-log2)
//!            block-crc  = CRC-64/XZ of (block index as u64le, the 9 header
//!                         bytes, payload); the index starts at 0 in each
//!                         member, so dropped, duplicated or reordered blocks
//!                         are caught
//! payload, without long-repeat removal:
//!            kind 1 = coded:  an arithmetic code string for raw-len bytes
//!                             (packed-len < raw-len)
//!            kind 2 = stored: the raw bytes (packed-len = raw-len)
//! payload, with long-repeat removal (packed-len <= raw-len + raw-len/8 + 64):
//!            cmds-len:varint cmds[cmds-len] residual-len:varint body
//!            kind 1 = body is a code string for residual-len bytes
//!            kind 2 = body is the residual-len residual bytes
//!            cmds   := 0 raw | 1 coded   (a selector byte, then:)
//!            raw    := (literal-run:varint copy-len:varint distance:varint)*
//!                      literal-run:varint
//!            coded  := the same commands, range coded (see `crate::dedup`):
//!                      the number of copies, then per copy its literal run,
//!                      its distance or which of the last four it repeats,
//!                      and its length less the minimum; then the final
//!                      literal run. The encoder writes whichever is smaller.
//!            The block is rebuilt in order: each literal run takes the next
//!            bytes of the residual; each copy repeats copy-len bytes from
//!            distance bytes back in the output (0 < distance <= min(window,
//!            bytes produced so far), byte by byte, so a copy may overlap
//!            itself). The residual must be consumed exactly and the block
//!            must come to exactly raw-len bytes.
//! varint  := unsigned LEB128, at most 10 bytes, no overlong encodings
//! end     := kind 0
//! trailer := data-crc (8 bytes LE: CRC-64/XZ of the member's original data)
//!            length   (8 bytes LE: original size)
//! stream  := member+
//! ```
//!
//! Models 0, 1 and 3 use the bit-wise coder; models 2 and 4 the byte-wise
//! binary range coder. Every coded block (or residual) starts from a freshly
//! initialised model.
//!
//! **Parallel blocks.** Every block's model starts afresh and the framing
//! gives each block's sizes, so the model-coded parts of blocks are
//! compressed and decompressed on separate threads. With long-repeat removal,
//! the copies are found and applied in stream order (they refer to earlier
//! output), around the parallel model stages. The output is identical
//! whatever the thread count.
//!
//! **Expansion.** A block that would not shrink is stored, so incompressible
//! data grows by at most 17 bytes per block (9 header + 8 CRC) plus 33 per
//! member (16 header + 1 end + 16 trailer). With long-repeat removal a block
//! also carries its command list and two lengths; with no copies that
//! envelope is at most 12 bytes (a one-byte list length, the selector, one
//! literal-run varint, and the residual length), and each copy adds at most
//! 16 bytes while removing at least 256, so the bound is 29 bytes per block
//! plus 33 per member.
//!
//! **Streaming.** Both directions read and write incrementally, with at most
//! about `2 × threads` blocks in flight, so memory is bounded and the tool
//! works as a pipeline transducer. Concatenated members decode to the
//! concatenation of their contents.
//!
//! **Integrity.** The header has its own CRC-64, so foreign or damaged
//! files are rejected before decoding. Every block carries a CRC-64 over its
//! position, framing and payload, checked *before* the block is decoded or
//! any size-dependent memory is allocated. The data CRC-64 sits in the
//! trailer, because a streaming compressor only knows it at end of input.
//! Each coded block must also end canonically and consume exactly
//! `packed-len` bytes.
//!
//! **Bounded memory.** The decoder works out, from the header alone, how
//! much memory each in-flight block needs (payload, output and model), and
//! limits the number of workers so the total stays within
//! [`DecodeOptions::memory`]. A stream that cannot be decoded within the
//! budget even on one thread is rejected before allocating. Payloads are read
//! incrementally, so a forged length on a truncated stream cannot force a
//! large allocation.
//!
//! **Portability.** Every field has a fixed width and byte order, and the
//! models use only integer arithmetic, so streams are identical on every
//! platform (any endianness, any word size ≥ 32 bits).

use std::collections::BTreeMap;
use std::io::{self, BufRead, Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use crate::crc64::Crc64;
use crate::dedup::{get_varint, pack_commands, put_varint, unpack_commands, MAX_VARINT_LEN};
use crate::models::{decode_block, encode_block, Order};

/// Magic number at the start of every member (last byte = format version).
pub const MAGIC: [u8; 4] = *b"ACZ\x0C";
const HEADER_LEN: usize = 16;
const TRAILER_LEN: usize = 16;
const BLOCK_HEADER_LEN: usize = 9;
const BLOCK_CRC_LEN: usize = 8;

const KIND_END: u8 = 0;
const KIND_CODED: u8 = 1;
const KIND_STORED: u8 = 2;

/// Header flag: blocks carry long-repeat commands (see [`crate::dedup`]).
const FLAG_DEDUP: u8 = 1;

/// Smallest permitted block size (log2): 64 KiB.
pub const MIN_BLOCK_LOG2: u8 = 16;
/// Largest permitted block size (log2): 1 GiB.
pub const MAX_BLOCK_LOG2: u8 = 30;
// Positions inside a block are stored as u32 by the CM match model; a block
// must therefore stay below 4 GiB. Raising MAX_BLOCK_LOG2 past 31 is a
// compile error, not a silent wrap.
const _: () = assert!(MAX_BLOCK_LOG2 <= 31, "block positions must fit in u32");

/// Default block size: 16 MiB.
pub const DEFAULT_BLOCK_LOG2: u8 = 24;

/// Default memory budget for compression and decompression: 4 GiB.
pub const DEFAULT_MEMORY: u64 = 4 << 30;

/// Default long-repeat window: 256 MiB.
pub const DEFAULT_DEDUP_LOG2: u8 = 28;
/// Largest long-repeat window: 64 GiB.
pub const MAX_DEDUP_LOG2: u8 = 36;

/// Hard upper limit on worker threads, whatever the caller asks for.
pub const MAX_THREADS: usize = 1024;

/// Compression settings.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Model to use.
    pub order: Order,
    /// Block size is `2^block_log2` bytes.
    pub block_log2: u8,
    /// Maximum worker threads (≥ 1); fewer are used if `memory` requires.
    pub threads: usize,
    /// Memory budget in bytes for in-flight blocks and models.
    pub memory: u64,
    /// Long-repeat removal window (log2 bytes); 0 disables it. Must exceed
    /// `block_log2`.
    pub dedup_log2: u8,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            order: Order::Mix,
            block_log2: DEFAULT_BLOCK_LOG2,
            threads: default_threads(),
            memory: DEFAULT_MEMORY,
            dedup_log2: 0,
        }
    }
}

/// Decompression settings.
#[derive(Clone, Copy, Debug)]
pub struct DecodeOptions {
    /// Maximum worker threads (≥ 1); fewer are used if `memory` requires.
    pub threads: usize,
    /// Memory budget in bytes for in-flight blocks and models.
    pub memory: u64,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        DecodeOptions { threads: default_threads(), memory: DEFAULT_MEMORY }
    }
}

/// Fixed allowance added to the long-repeat memory bounds: their constant
/// terms (varint lengths, the 64-byte payload margin) are under 128 bytes,
/// and the rest absorbs allocator rounding.
const SLACK: u64 = 4096;

/// Upper bound on the buffer memory one in-flight block needs.
///
/// Without long-repeat removal, every buffer's requested capacity is
/// bounded by one block: input blocks are allocated at exactly the block size, trial code
/// strings are capped below the raw size ([`encode_block`]), payloads grow
/// at most to their declared length, and decoded output is allocated
/// exactly; a block holds at most two such buffers at once.
///
/// With long-repeat removal (block size B), a block holds at most, at any
/// one stage:
/// * decoding worker: payload (accepted only up to 1.125·B + 64), a copy of
///   its command list (no larger than the payload) and the residual (≤ B);
/// * in-order consumer: command list, residual, and the rebuilt block (B);
/// * encoding worker: residual (≤ B), command list (≤ B/8 + 10: it is
///   allocated once with capacity 30·⌊B/256⌋ + 10, enough for three
///   maximal 10-byte varints per copy of at least 256 bytes, so it never
///   grows), a trial coding of it (a bounded buffer that never asks for
///   more than the list), its stored form (≤ B/8 + 11: the smaller of the
///   two, after a selector byte), coded body (< residual) and the assembled
///   payload (≤ body + commands + 21).
///
/// Each is below 3.25·B + 128 in *requested* capacity: buffers are
/// allocated with exact requests (`with_capacity`, `reserve_exact`, or
/// clamped growth), and the allocator may round a request up slightly,
/// which [`SLACK`] covers.
fn buffer_cost(block_size: usize, dedup: bool) -> u64 {
    let b = block_size as u64;
    if dedup {
        b * 13 / 4 + SLACK
    } else {
        2 * b
    }
}

/// Number of workers that fit in `memory`, at most `min(threads,
/// MAX_THREADS)`. With `t > 1` workers, up to `2t` blocks are in flight (one
/// buffer pair each) and `t` models are live: `t × (2·buf + model)`. With
/// one worker, one block and one model: `buf + model`.
fn workers_for(order: Order, block_size: usize, threads: usize, memory: u64, dedup: bool) -> io::Result<usize> {
    let model = crate::models::model_memory(order, block_size);
    let buf = buffer_cost(block_size, dedup);
    let single = buf.saturating_add(model);
    if single > memory {
        let what =
            format!("stream needs about {} MiB (block {} MiB, model {})", single >> 20, block_size >> 20, order.name());
        return Err(out_of_memory(&what, memory));
    }
    let per_worker = buf.saturating_mul(2).saturating_add(model);
    let fit = (memory / per_worker).max(1);
    let wanted = threads.clamp(1, MAX_THREADS) as u64;
    Ok(wanted.min(fit) as usize)
}

/// Number of hardware threads available to this process. Portable
/// (`std::thread::available_parallelism`); on Linux it respects cgroup
/// CPU quotas and affinity masks. Falls back to 1.
pub fn default_threads() -> usize {
    thread::available_parallelism().map_or(1, |n| n.get())
}

/// Byte counts from one compression or decompression run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Uncompressed bytes.
    pub raw: u64,
    /// Compressed bytes.
    pub packed: u64,
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn truncated() -> io::Error {
    invalid("compressed data is truncated")
}

fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

/// Fill `buf` completely, or fail as truncated input.
fn read_exact_or_truncated<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<()> {
    if read_full(r, buf)? < buf.len() {
        return Err(truncated());
    }
    Ok(())
}

fn block_header(kind: u8, raw_len: usize, packed_len: usize) -> [u8; BLOCK_HEADER_LEN] {
    let mut bh = [0u8; BLOCK_HEADER_LEN];
    bh[0] = kind;
    bh[1..5].copy_from_slice(&(raw_len as u32).to_le_bytes());
    bh[5..9].copy_from_slice(&(packed_len as u32).to_le_bytes());
    bh
}

/// A block between the parallel stage and the sequential long-repeat
/// stage: its raw length, its command list (with long-repeat removal) and
/// its data (the residual when there are commands).
struct Frame {
    raw_len: usize,
    cmds: Option<Vec<u8>>,
    data: Vec<u8>,
}

fn block_crc(index: u64, header: &[u8; BLOCK_HEADER_LEN], payload: &[u8]) -> u64 {
    let mut c = Crc64::new();
    c.update(&index.to_le_bytes());
    c.update(header);
    c.update(payload);
    c.value()
}

/// The long-repeat window for exponent `log2` (0 = none), validated before
/// any shift: it must exceed the block size, be at most
/// [`MAX_DEDUP_LOG2`], and be representable on this platform (a 2^32
/// window cannot be used where `usize` has 32 bits).
fn window_size(log2: u8, block_log2: u8) -> io::Result<Option<usize>> {
    if log2 == 0 {
        return Ok(None);
    }
    if log2 <= block_log2 || log2 > MAX_DEDUP_LOG2 {
        return Err(invalid(format!("invalid long-repeat window 2^{log2}")));
    }
    // Allocations are limited to isize::MAX bytes, so a 2^31 window is
    // unusable on a 32-bit target even though it fits in usize.
    1usize.checked_shl(u32::from(log2)).filter(|&w| w <= isize::MAX as usize).map(Some).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("long-repeat window 2^{log2} is not supported on this platform"),
        )
    })
}

/// The memory budget, capped at what this platform can address.
fn usable(memory: u64) -> u64 {
    memory.min(isize::MAX as u64)
}

/// Memory left for block workers after the long-repeat window (and, when
/// encoding, the anchor table) is set aside.
fn after_dedup(memory: u64, dedup: Option<usize>, side: Side, block_size: usize) -> io::Result<u64> {
    let Some(w) = dedup else { return Ok(memory) };
    // The window (and the anchor table when encoding), plus the sequential
    // stage's scratch: while matching, the input block, its residual and
    // command list (≤ 2.25·B); while rebuilding, the output block in
    // progress (counted in the per-block bound) and the ring itself.
    let scratch = (block_size as u64) * 9 / 4 + SLACK;
    let need = match side {
        Side::Encode => crate::dedup::Deduper::memory(w),
        Side::Decode => w as u64,
    } + scratch;
    memory
        .checked_sub(need)
        .filter(|&m| m > 0)
        .ok_or_else(|| out_of_memory(&format!("long-repeat window needs {} MiB", need >> 20), memory))
}

fn out_of_memory(what: &str, memory: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::OutOfMemory,
        format!("{what} but the memory limit is {} MiB; raise it with --memory", memory >> 20),
    )
}

/// Which side of the coder a memory estimate is for.
#[derive(Clone, Copy)]
enum Side {
    Encode,
    Decode,
}

/// Validate a stream's long-repeat window and decide, before anything is
/// allocated, how many workers fit in `memory`. Returns the window (if
/// any) and the worker count.
fn plan(
    order: Order,
    block_log2: u8,
    dedup_log2: u8,
    threads: usize,
    memory: u64,
    side: Side,
) -> io::Result<(Option<usize>, usize)> {
    let block_size = 1usize << block_log2;
    let dedup = window_size(dedup_log2, block_log2)?;
    let memory = after_dedup(usable(memory), dedup, side, block_size)?;
    let threads = workers_for(order, block_size, threads, memory, dedup.is_some())?;
    Ok((dedup, threads))
}

fn header(opts: &Options) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..4].copy_from_slice(&MAGIC);
    h[4] = opts.order.tag();
    h[5] = if opts.dedup_log2 > 0 { FLAG_DEDUP } else { 0 };
    h[6] = opts.block_log2;
    h[7] = opts.dedup_log2;
    let crc = Crc64::of(&h[..8]);
    h[8..].copy_from_slice(&crc.to_le_bytes());
    h
}

/// Run `work` over the jobs produced by `produce` on `threads` workers and
/// deliver the results to `consume` in the order the jobs were produced.
/// `produce` returns `Ok(None)` when there are no more jobs. The first
/// error from any stage stops the pipeline and is returned.
///
/// `produce` and `consume` run on the calling thread, one job at a time and
/// in order; only `work` runs in parallel. That is what lets a stage that
/// carries state from one block to the next (reading the input, the
/// long-repeat history, writing the output) stay sequential while the
/// expensive modelling runs on every core. Each job carries a sequence
/// number, and results that finish early wait in `pending` until all
/// earlier ones have been consumed.
///
/// At most `2 × threads` jobs are in flight, counting those finished but
/// waiting for an earlier one: enough to keep every worker busy while one
/// slow block holds up the output, and a fixed bound, which is what the
/// memory admission in [`workers_for`] counts on.
fn ordered_pipeline<J, T, P, W, C>(threads: usize, mut produce: P, work: W, mut consume: C) -> io::Result<()>
where
    J: Send,
    T: Send,
    P: FnMut() -> io::Result<Option<J>>,
    W: Fn(J) -> io::Result<T> + Sync,
    C: FnMut(T) -> io::Result<()>,
{
    if threads <= 1 {
        // One block in flight at a time.
        while let Some(job) = produce()? {
            consume(work(job)?)?;
        }
        return Ok(());
    }
    thread::scope(|s| {
        let (job_tx, job_rx) = mpsc::sync_channel::<(u64, J)>(threads);
        let job_rx = Arc::new(Mutex::new(job_rx));
        let (res_tx, res_rx) = mpsc::channel::<(u64, io::Result<T>)>();
        let work = &work;
        for _ in 0..threads {
            let rx = Arc::clone(&job_rx);
            let tx = res_tx.clone();
            s.spawn(move || loop {
                // Hold the lock only while receiving the next job.
                let next = match rx.lock() {
                    Ok(guard) => guard.recv(),
                    Err(_) => break,
                };
                let Ok((seq, job)) = next else { break };
                // A panicking job must still send a result: the main thread
                // waits for every sequence number, so a lost one would hang it.
                let r = panic::catch_unwind(AssertUnwindSafe(|| work(job)))
                    .unwrap_or_else(|_| Err(io::Error::other("internal error: a worker thread panicked")));
                if tx.send((seq, r)).is_err() {
                    break;
                }
            });
        }
        drop(res_tx);

        let mut pending: BTreeMap<u64, T> = BTreeMap::new();
        let mut next_out = 0u64;
        let mut submitted = 0u64;
        let limit = 2 * threads as u64;

        // Take one result (waiting for it, or only if one is ready) and
        // consume every result that is now next in order. Returns false if
        // there was none: with `blocking`, that means every worker is gone.
        let mut handle = |blocking: bool, pending: &mut BTreeMap<u64, T>, next_out: &mut u64| -> io::Result<bool> {
            let msg = if blocking { res_rx.recv().ok() } else { res_rx.try_recv().ok() };
            let Some((seq, r)) = msg else { return Ok(false) };
            pending.insert(seq, r?);
            while let Some(t) = pending.remove(next_out) {
                consume(t)?;
                *next_out += 1;
            }
            Ok(true)
        };

        let result = (|| {
            while let Some(job) = produce()? {
                job_tx.send((submitted, job)).map_err(|_| io::Error::other("worker pool stopped"))?;
                submitted += 1;
                while submitted - next_out >= limit {
                    if !handle(true, &mut pending, &mut next_out)? {
                        return Err(io::Error::other("worker thread failed"));
                    }
                }
                while handle(false, &mut pending, &mut next_out)? {}
            }
            Ok(())
        })();
        // Closing the job channel lets idle workers exit.
        drop(job_tx);
        result?;
        while next_out < submitted {
            if !handle(true, &mut pending, &mut next_out)? {
                return Err(io::Error::other("worker thread failed"));
            }
        }
        Ok(())
    })
}

/// Compress everything from `input` into one member written to `output`.
///
/// The input is cut into blocks of the block size. Reading a block, and
/// removing long repeats from it, happen in order, because the long-repeat
/// history runs through the whole input. Each block is then coded with a
/// fresh model, in parallel, and stored instead if coding would not make
/// it smaller. The blocks are written in order, each with its header and
/// a checksum over its index, header and payload, and the member ends
/// with a checksum and the length of all the data.
///
/// `output` should be buffered. It is flushed before returning.
///
/// # Panics
///
/// Panics if `opts.block_log2` is outside `MIN_BLOCK_LOG2..=MAX_BLOCK_LOG2`.
pub fn compress<R: Read, W: Write>(mut input: R, mut output: W, opts: &Options) -> io::Result<Stats> {
    assert!((MIN_BLOCK_LOG2..=MAX_BLOCK_LOG2).contains(&opts.block_log2), "block size out of range");
    let block_size = 1usize << opts.block_log2;
    let order = opts.order;
    let (dedup, threads) = plan(order, opts.block_log2, opts.dedup_log2, opts.threads, opts.memory, Side::Encode)?;
    let mut deduper = dedup.map(crate::dedup::Deduper::new);
    output.write_all(&header(opts))?;
    let mut packed = HEADER_LEN as u64;

    let mut crc = Crc64::new();
    let mut raw = 0u64;
    let mut index = 0u64;
    ordered_pipeline(
        threads,
        || {
            let mut buf = vec![0u8; block_size];
            let n = read_full(&mut input, &mut buf)?;
            if n == 0 {
                return Ok(None);
            }
            buf.truncate(n);
            crc.update(&buf);
            raw += n as u64;
            Ok(Some(match deduper.as_mut() {
                Some(d) => {
                    let (residual, cmds) = d.frame(&buf);
                    Frame { raw_len: n, cmds: Some(cmds), data: residual }
                }
                None => Frame { raw_len: n, cmds: None, data: buf },
            }))
        },
        |Frame { raw_len, cmds, data }| {
            let residual_len = data.len();
            let (kind, body) = match encode_block(order, &data) {
                Some(code) => (KIND_CODED, code),
                None => (KIND_STORED, data),
            };
            let payload = match cmds {
                None => body,
                Some(cmds) => {
                    // cmds-len, cmds, residual-len, residual body.
                    let cmds = pack_commands(&cmds);
                    let mut p = Vec::with_capacity(cmds.len() + body.len() + 2 * MAX_VARINT_LEN);
                    put_varint(cmds.len() as u64, &mut p);
                    p.extend_from_slice(&cmds);
                    put_varint(residual_len as u64, &mut p);
                    p.extend_from_slice(&body);
                    p
                }
            };
            Ok((kind, raw_len, payload))
        },
        |(kind, raw_len, payload): (u8, usize, Vec<u8>)| {
            let bh = block_header(kind, raw_len, payload.len());
            let crc = block_crc(index, &bh, &payload);
            index += 1;
            output.write_all(&bh)?;
            output.write_all(&payload)?;
            output.write_all(&crc.to_le_bytes())?;
            packed += (BLOCK_HEADER_LEN + payload.len() + BLOCK_CRC_LEN) as u64;
            Ok(())
        },
    )?;
    output.write_all(&[KIND_END])?;
    output.write_all(&crc.value().to_le_bytes())?;
    output.write_all(&raw.to_le_bytes())?;
    packed += 1 + TRAILER_LEN as u64;
    output.flush()?;
    Ok(Stats { raw, packed })
}

/// `BufRead` adaptor counting consumed bytes.
struct Counted<R> {
    inner: R,
    count: u64,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count += n as u64;
        Ok(n)
    }
}

impl<R: BufRead> BufRead for Counted<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }
    fn consume(&mut self, amt: usize) {
        self.count += amt as u64;
        self.inner.consume(amt);
    }
}

/// Decompress all members from `input` using `threads` workers, writing
/// the data to `output` in order.
///
/// Data is written as it is decoded, so on corrupt input some output may
/// already have been produced before the error is reported. `output` is
/// flushed before returning successfully.
pub fn decompress<R: BufRead, W: Write>(input: R, mut output: W, opts: &DecodeOptions) -> io::Result<Stats> {
    let mut input = Counted { inner: input, count: 0 };
    let mut raw_total = decompress_member(&mut input, &mut output, true, opts)?;
    while !input.fill_buf()?.is_empty() {
        raw_total += decompress_member(&mut input, &mut output, false, opts)?;
    }
    output.flush()?;
    Ok(Stats { raw: raw_total, packed: input.count })
}

/// Read and check a member header; returns the model, the block size
/// (log2) and the long-repeat window (log2, 0 for none).
fn read_header<R: Read>(input: &mut R, first: bool) -> io::Result<(Order, u8, u8)> {
    let mut h = [0u8; HEADER_LEN];
    let n = read_full(input, &mut h)?;
    // The magic without its version byte identifies the format.
    let id = MAGIC.len() - 1;
    if n < id || h[..id] != MAGIC[..id] {
        return Err(invalid(if first { "not in acz format" } else { "trailing garbage after acz data" }));
    }
    if n < MAGIC.len() {
        return Err(truncated());
    }
    if h[id] != MAGIC[id] {
        return Err(invalid(format!("unsupported acz format version {}", h[3])));
    }
    if n < HEADER_LEN {
        return Err(truncated());
    }
    let want = u64::from_le_bytes(h[8..].try_into().expect("8 bytes"));
    if Crc64::of(&h[..8]) != want {
        return Err(invalid("corrupt header (CRC mismatch)"));
    }
    if h[5] & !FLAG_DEDUP != 0 || (h[5] & FLAG_DEDUP == 0 && h[7] != 0) {
        return Err(invalid("unsupported header flags"));
    }
    let order = Order::from_tag(h[4]).ok_or_else(|| invalid(format!("unknown model id {}", h[4])))?;
    if !(MIN_BLOCK_LOG2..=MAX_BLOCK_LOG2).contains(&h[6]) {
        return Err(invalid(format!("invalid block size 2^{}", h[6])));
    }
    if h[5] & FLAG_DEDUP != 0 && h[7] == 0 {
        return Err(invalid("corrupt header (long-repeat flag without a window)"));
    }
    Ok((order, h[6], h[7]))
}

/// Decompress one member, the mirror of [`compress`].
///
/// The header fixes the model, block size and long-repeat window, and so
/// the number of workers, before model or history buffers are allocated.
/// Reading a block is sequential: its header is checked against the
/// limits the encoder obeys (which also bounds what a forged header can
/// make the decoder allocate), its payload read, and its checksum
/// verified. Each block is
/// then decoded in parallel. Rebuilding long repeats and writing the
/// output are sequential again, since copies refer to earlier output.
/// Finally the trailer's length and checksum must match the data.
fn decompress_member<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    first: bool,
    opts: &DecodeOptions,
) -> io::Result<u64> {
    let (order, block_log2, dedup_log2) = read_header(input, first)?;
    let block_size = 1usize << block_log2;
    let (dedup, threads) = plan(order, block_log2, dedup_log2, opts.threads, opts.memory, Side::Decode)?;
    let mut rebuild = dedup.map(crate::dedup::Reconstructor::new);
    let mut index = 0u64;

    let mut crc = Crc64::new();
    let mut raw = 0u64;
    let mut ended = false;
    ordered_pipeline(
        threads,
        || {
            if ended {
                return Ok(None);
            }
            let mut bh = [0u8; BLOCK_HEADER_LEN];
            read_exact_or_truncated(input, &mut bh[..1])?;
            let kind = bh[0];
            if kind == KIND_END {
                ended = true;
                return Ok(None);
            }
            read_exact_or_truncated(input, &mut bh[1..])?;
            let raw_len = u32::from_le_bytes(bh[1..5].try_into().expect("4 bytes")) as usize;
            let packed_len = u32::from_le_bytes(bh[5..9].try_into().expect("4 bytes")) as usize;
            // The encoder only emits a coded block when it is smaller than
            // the raw data; this also bounds allocations on corrupt input.
            // With long-repeat commands, a frame's payload is its residual
            // (at most raw_len) plus a command list of at most a few bytes
            // per 256 copied bytes; bound it generously but finitely.
            let ok = raw_len >= 1
                && raw_len <= block_size
                && match (kind, dedup.is_some()) {
                    (KIND_CODED, false) => packed_len < raw_len,
                    (KIND_STORED, false) => packed_len == raw_len,
                    (KIND_CODED | KIND_STORED, true) => packed_len <= raw_len + raw_len / 8 + 64,
                    _ => false,
                };
            if !ok {
                return Err(invalid(format!("corrupt block header (kind {kind}, raw {raw_len}, packed {packed_len})")));
            }
            // Read incrementally: requested capacity grows (geometrically,
            // never past packed_len) only as data actually arrives, so a
            // forged length on a truncated stream cannot force a large
            // allocation.
            let mut payload: Vec<u8> = Vec::new();
            while payload.len() < packed_len {
                let avail = input.fill_buf()?;
                if avail.is_empty() {
                    return Err(truncated());
                }
                let n = avail.len().min(packed_len - payload.len());
                let need = payload.len() + n;
                crate::models::reserve_bounded(&mut payload, need, packed_len);
                payload.extend_from_slice(&avail[..n]);
                input.consume(n);
            }
            let mut crc = [0u8; BLOCK_CRC_LEN];
            read_exact_or_truncated(input, &mut crc)?;
            if u64::from_le_bytes(crc) != block_crc(index, &bh, &payload) {
                return Err(invalid(format!("corrupt block {index} (CRC mismatch)")));
            }
            index += 1;
            Ok(Some((kind, raw_len, payload)))
        },
        |(kind, raw_len, mut payload): (u8, usize, Vec<u8>)| {
            // The block's data, `len` bytes once decoded.
            let decode = |body: Vec<u8>, len: usize| -> io::Result<Vec<u8>> {
                if kind == KIND_STORED {
                    if body.len() != len {
                        return Err(invalid("corrupt block (stored length mismatch)"));
                    }
                    return Ok(body);
                }
                if len == 0 {
                    return Err(invalid("corrupt block (empty coded residual)"));
                }
                decode_block(order, &body, len).map_err(|e| {
                    if e.kind() == io::ErrorKind::UnexpectedEof {
                        invalid("corrupt block (code string too short)")
                    } else {
                        e
                    }
                })
            };
            if dedup.is_none() {
                return Ok(Frame { raw_len, cmds: None, data: decode(payload, raw_len)? });
            }
            let mut pos = 0usize;
            let cmds_len = get_varint(&payload, &mut pos)?;
            let cmds_end = usize::try_from(cmds_len)
                .ok()
                .and_then(|l| pos.checked_add(l))
                .filter(|&e| e <= payload.len())
                .ok_or_else(|| invalid("corrupt block (command list length)"))?;
            let cmds = unpack_commands(&payload[pos..cmds_end], raw_len)?;
            pos = cmds_end;
            let res_len = usize::try_from(get_varint(&payload, &mut pos)?)
                .ok()
                .filter(|&l| l <= raw_len)
                .ok_or_else(|| invalid("corrupt block (residual length)"))?;
            payload.drain(..pos);
            let residual = decode(payload, res_len)?;
            Ok(Frame { raw_len, cmds: Some(cmds), data: residual })
        },
        |Frame { raw_len, cmds, data }| {
            let data = match (cmds, rebuild.as_mut()) {
                (Some(cmds), Some(r)) => r.frame(&data, &cmds, raw_len)?,
                _ => data,
            };
            crc.update(&data);
            raw += data.len() as u64;
            output.write_all(&data)
        },
    )?;

    let mut trailer = [0u8; TRAILER_LEN];
    if read_full(input, &mut trailer)? < TRAILER_LEN {
        return Err(truncated());
    }
    let want_crc = u64::from_le_bytes(trailer[..8].try_into().expect("8 bytes"));
    let want_len = u64::from_le_bytes(trailer[8..].try_into().expect("8 bytes"));
    if want_len != raw {
        return Err(invalid(format!("length mismatch: expected {want_len} bytes, decoded {raw}")));
    }
    if want_crc != crc.value() {
        return Err(invalid(format!("data CRC mismatch: expected {want_crc:016x}, computed {:016x}", crc.value())));
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDERS: [Order; 5] = [Order::Zero, Order::One, Order::Two, Order::Ppm, Order::Mix];

    fn opts(order: Order, block_log2: u8, threads: usize) -> Options {
        Options { order, block_log2, threads, memory: DEFAULT_MEMORY, dedup_log2: 0 }
    }

    fn dopts(threads: usize) -> DecodeOptions {
        DecodeOptions { threads, memory: DEFAULT_MEMORY }
    }

    /// Recompute a modified header's CRC, so the change reaches the checks
    /// behind it.
    fn reseal(h: &mut [u8]) {
        let crc = Crc64::of(&h[..8]);
        h[8..HEADER_LEN].copy_from_slice(&crc.to_le_bytes());
    }

    fn roundtrip_with(data: &[u8], o: &Options) -> Vec<u8> {
        let mut packed = Vec::new();
        let st = compress(data, &mut packed, o).unwrap();
        assert_eq!(st.raw, data.len() as u64);
        assert_eq!(st.packed, packed.len() as u64);
        let mut out = Vec::new();
        let st2 = decompress(&packed[..], &mut out, &dopts(o.threads)).unwrap();
        assert_eq!(out, data);
        assert_eq!(st2.packed, packed.len() as u64);
        packed
    }

    fn roundtrip(data: &[u8], order: Order) -> usize {
        roundtrip_with(data, &opts(order, DEFAULT_BLOCK_LOG2, 1)).len()
    }

    fn lcg(seed: &mut u64) -> u64 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *seed >> 33
    }

    fn text(n: usize, seed: u64) -> Vec<u8> {
        let words = ["the ", "quick ", "brown ", "fox ", "jumps ", "over ", "lazy ", "dog ", "\n"];
        let mut s = seed;
        let mut data = Vec::new();
        while data.len() < n {
            data.extend_from_slice(words[(lcg(&mut s) % words.len() as u64) as usize].as_bytes());
        }
        data.truncate(n);
        data
    }

    #[test]
    fn empty_and_tiny() {
        for o in ORDERS {
            for d in [&b""[..], b"a", &[0], &[255], b"ab"] {
                roundtrip(d, o);
            }
        }
    }

    #[test]
    fn random_bytes_are_stored() {
        let mut s = 1;
        let data: Vec<u8> = (0..200_000).map(|_| lcg(&mut s) as u8).collect();
        for o in ORDERS {
            let n = roundtrip(&data, o);
            // One stored block: header 16 + block header 9 + block CRC 8
            // + end 1 + trailer 16.
            assert!(n <= data.len() + 50, "{o:?}: {n}");
        }
    }

    #[test]
    fn constant_compresses_hard() {
        let data = vec![b'x'; 1_000_000];
        for o in ORDERS {
            let n = roundtrip(&data, o);
            assert!(n < 2_000, "{o:?}: {n}");
        }
    }

    #[test]
    fn text_like() {
        let data = text(300_000, 7);
        for o in ORDERS {
            let n = roundtrip(&data, o);
            let limit = if o == Order::Zero { data.len() * 6 / 10 } else { data.len() / 4 };
            assert!(n < limit, "{o:?}: {n}");
        }
    }

    #[test]
    fn every_length_small() {
        let mut s = 3;
        for len in 0..300 {
            let data: Vec<u8> = (0..len).map(|_| (lcg(&mut s) % 7) as u8 * 37).collect();
            for o in [Order::Zero, Order::One, Order::Two] {
                roundtrip(&data, o);
            }
        }
    }

    #[test]
    fn output_is_independent_of_thread_count() {
        // Several 64 KiB blocks, including a short final one.
        let data = text(5 * 65536 + 1234, 11);
        for o in [Order::One, Order::Two, Order::Ppm, Order::Mix] {
            let one = roundtrip_with(&data, &opts(o, 16, 1));
            for t in [2, 3, 8] {
                let many = roundtrip_with(&data, &opts(o, 16, t));
                assert!(one == many, "{o:?}: {t} threads changed the output");
            }
            let mut out = Vec::new();
            decompress(&one[..], &mut out, &dopts(5)).unwrap();
            assert_eq!(out, data);
        }
    }

    #[test]
    fn exact_block_multiples() {
        for len in [65536, 2 * 65536, 2 * 65536 - 1, 2 * 65536 + 1] {
            roundtrip_with(&text(len, len as u64), &opts(Order::One, 16, 4));
        }
    }

    #[test]
    fn a_panicking_worker_is_an_error_not_a_hang() {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut n = 0u32;
            let r = ordered_pipeline(
                3,
                || {
                    n += 1;
                    Ok((n <= 40).then_some(n))
                },
                |j: u32| if j == 5 { panic!("job {j}") } else { Ok(j) },
                |_| Ok(()),
            );
            tx.send(r.is_err()).unwrap();
        });
        let failed = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("pipeline hung");
        assert!(failed);
    }

    #[test]
    fn concatenated_members() {
        let mut packed = Vec::new();
        compress(&b"hello, "[..], &mut packed, &opts(Order::One, 24, 1)).unwrap();
        compress(&b""[..], &mut packed, &opts(Order::Zero, 16, 1)).unwrap();
        compress(&b"world\n"[..], &mut packed, &opts(Order::Mix, 20, 2)).unwrap();
        let mut out = Vec::new();
        decompress(&packed[..], &mut out, &dopts(3)).unwrap();
        assert_eq!(out, b"hello, world\n");
    }

    #[test]
    fn header_errors_are_specific() {
        let mut packed = Vec::new();
        compress(&text(5000, 1)[..], &mut packed, &opts(Order::One, 24, 1)).unwrap();
        let msg = |b: &[u8]| decompress(b, &mut Vec::new(), &dopts(1)).unwrap_err().to_string();
        let mut v = packed.clone();
        v[3] = 8;
        assert!(msg(&v).contains("unsupported acz format version 8"));
        // An unknown model id is rejected in a header with a valid CRC.
        let mut m4 = packed.clone();
        m4[4] = 5;
        reseal(&mut m4);
        assert!(msg(&m4).contains("unknown model id 5"));
        let mut m = packed.clone();
        m[4] = 2;
        assert!(msg(&m).contains("corrupt header"));
        let mut c = packed.clone();
        c[9] ^= 0x40;
        assert!(msg(&c).contains("corrupt header"));
        assert!(msg(&packed[..10]).contains("truncated"));
        let mut d = packed.clone();
        let n = d.len();
        d[n - 12] ^= 1; // inside the data CRC
        assert!(msg(&d).contains("data CRC mismatch"));
        let mut k = packed.clone();
        k[HEADER_LEN] = 9; // block kind
        assert!(msg(&k).contains("corrupt block header"));
    }

    /// Split a single-member stream into (header, blocks, tail) byte ranges.
    fn split_blocks(packed: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>, Vec<u8>) {
        let mut pos = HEADER_LEN;
        let mut blocks = Vec::new();
        while packed[pos] != KIND_END {
            let len = u32::from_le_bytes(packed[pos + 5..pos + 9].try_into().unwrap()) as usize;
            let end = pos + BLOCK_HEADER_LEN + len + BLOCK_CRC_LEN;
            blocks.push(packed[pos..end].to_vec());
            pos = end;
        }
        (packed[..HEADER_LEN].to_vec(), blocks, packed[pos..].to_vec())
    }

    fn join(h: &[u8], blocks: &[Vec<u8>], t: &[u8]) -> Vec<u8> {
        let mut v = h.to_vec();
        for b in blocks {
            v.extend_from_slice(b);
        }
        v.extend_from_slice(t);
        v
    }

    #[test]
    fn reordered_duplicated_or_dropped_blocks_are_caught() {
        let data = text(4 * 65536, 5);
        for o in [Order::One, Order::Ppm, Order::Mix] {
            let mut packed = Vec::new();
            compress(&data[..], &mut packed, &opts(o, 16, 2)).unwrap();
            let (h, blocks, t) = split_blocks(&packed);
            assert_eq!(blocks.len(), 4);
            assert_eq!(join(&h, &blocks, &t), packed);
            let msg = |v: Vec<u8>| decompress(&v[..], &mut Vec::new(), &dopts(2)).unwrap_err().to_string();

            let mut swapped = blocks.clone();
            swapped.swap(1, 2);
            assert!(msg(join(&h, &swapped, &t)).contains("corrupt block 1"));

            let mut dup = blocks.clone();
            dup.insert(2, blocks[1].clone());
            assert!(msg(join(&h, &dup, &t)).contains("corrupt block 2"));

            let mut dropped = blocks.clone();
            dropped.remove(1);
            assert!(msg(join(&h, &dropped, &t)).contains("corrupt block 1"));

            // Truncation exactly at every block boundary.
            for k in 0..blocks.len() {
                let cut = join(&h, &blocks[..k], &[]);
                assert!(msg(cut).contains("truncated"));
            }
        }
    }

    #[test]
    fn forged_length_cannot_force_allocation() {
        // A valid header claiming a 1 GiB block followed by a block header
        // that claims ~1 GiB of payload, but the stream then ends. The
        // decoder must report truncation without allocating 1 GiB.
        let o = Options { order: Order::Zero, block_log2: 30, threads: 1, memory: DEFAULT_MEMORY, dedup_log2: 0 };
        let mut v = header(&o).to_vec();
        v.push(KIND_STORED);
        v.extend_from_slice(&((1u32 << 30) - 1).to_le_bytes());
        v.extend_from_slice(&((1u32 << 30) - 1).to_le_bytes());
        v.extend_from_slice(b"only a few bytes");
        let err = decompress(&v[..], &mut Vec::new(), &dopts(1)).unwrap_err();
        assert!(err.to_string().contains("truncated"), "{err}");
    }

    #[test]
    fn memory_budget_limits_workers_and_refuses_oversized_streams() {
        let b16 = 1usize << 16;
        // Plenty of memory: all requested workers.
        assert_eq!(workers_for(Order::One, b16, 8, DEFAULT_MEMORY, false).unwrap(), 8);
        // CM needs about 156 MB per worker at 16 MiB blocks; 512 MiB allows
        // only a few.
        let t = workers_for(Order::Mix, 1 << 24, 12, 512 << 20, false).unwrap();
        assert!((1..=3).contains(&t), "{t}");
        // Long-repeat removal raises the per-block bound (3.25·B vs 2·B).
        let plain = workers_for(Order::Zero, 1 << 24, 64, 1 << 30, false).unwrap();
        let dedup = workers_for(Order::Zero, 1 << 24, 64, 1 << 30, true).unwrap();
        assert!(dedup < plain, "{dedup} vs {plain}");
        assert!(after_dedup(1 << 30, Some(1 << 28), Side::Decode, 1 << 24).unwrap() < (1 << 30) - (1 << 28));
        // Absurd thread requests are clamped, computed without overflow.
        assert_eq!(workers_for(Order::Zero, b16, usize::MAX, u64::MAX, false).unwrap(), MAX_THREADS);
        assert_eq!(workers_for(Order::Mix, 1 << 30, usize::MAX, u64::MAX, false).unwrap(), MAX_THREADS);
        // Not even one worker fits: refused with a clear message.
        let err = workers_for(Order::Mix, 1 << 30, 1, 1 << 30, false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
        assert!(err.to_string().contains("--memory"));

        // End to end: a CM stream with 1 GiB blocks is refused by a decoder
        // limited to 256 MiB, before allocating anything large.
        let o = Options { order: Order::Mix, block_log2: 30, threads: 1, memory: DEFAULT_MEMORY, dedup_log2: 0 };
        let mut packed = Vec::new();
        compress(&b"tiny"[..], &mut packed, &o).unwrap();
        let small = DecodeOptions { threads: 4, memory: 256 << 20 };
        let err = decompress(&packed[..], &mut Vec::new(), &small).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
        let mut out = Vec::new();
        decompress(&packed[..], &mut out, &dopts(1)).unwrap();
        assert_eq!(out, b"tiny");
    }

    fn dedup_opts(order: Order, threads: usize) -> Options {
        Options { order, block_log2: 16, threads, memory: DEFAULT_MEMORY, dedup_log2: 20 }
    }

    /// Text with long repeats near and far, plus noise.
    fn repetitive(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        let units: Vec<Vec<u8>> = (0..5).map(|k| text(700 + 300 * k, seed + k as u64)).collect();
        let mut v = Vec::new();
        while v.len() < n {
            if lcg(&mut s) % 3 == 0 {
                v.extend((0..500).map(|_| lcg(&mut s) as u8));
            } else {
                v.extend_from_slice(&units[(lcg(&mut s) % 5) as usize]);
            }
        }
        v.truncate(n);
        v
    }

    #[test]
    fn dedup_roundtrips_all_models_and_is_thread_independent() {
        let data = repetitive(5 * 65536 + 777, 9);
        for o in ORDERS {
            let one = roundtrip_with(&data, &dedup_opts(o, 1));
            let many = roundtrip_with(&data, &dedup_opts(o, 4));
            assert!(one == many, "{o:?}: thread count changed the output");
            let plain = roundtrip_with(&data, &opts(o, 16, 1));
            assert!(one.len() < plain.len() + 64, "{o:?}: dedup {} vs plain {}", one.len(), plain.len());
        }
        // Empty input and a single short block.
        for d in [&b""[..], b"x", &text(100, 1)[..]] {
            roundtrip_with(d, &dedup_opts(Order::Mix, 2));
        }
    }

    #[test]
    fn dedup_expansion_is_bounded() {
        let mut s = 77;
        for len in [0usize, 1, 1000, 65536, 5 * 65536 + 3] {
            let data: Vec<u8> = (0..len).map(|_| lcg(&mut s) as u8).collect();
            for o in ORDERS {
                let n = roundtrip_with(&data, &dedup_opts(o, 2)).len();
                let blocks = len.div_ceil(65536);
                // The documented bound: 29 bytes per block (the selector
                // byte included) plus 33 per member.
                assert!(n <= len + 29 * blocks + 33, "{o:?} len {len}: {n}");
            }
        }
    }

    #[test]
    fn dedup_streams_reject_corruption() {
        let data = repetitive(3 * 65536, 4);
        let mut packed = Vec::new();
        compress(&data[..], &mut packed, &dedup_opts(Order::One, 1)).unwrap();
        for cut in (0..packed.len()).step_by(97) {
            assert!(decompress(&packed[..cut], &mut Vec::new(), &dopts(1)).is_err(), "cut {cut}");
        }
        for i in (0..packed.len()).step_by(41) {
            let mut c = packed.clone();
            c[i] ^= 0x10;
            assert!(decompress(&c[..], &mut Vec::new(), &dopts(1)).is_err(), "flip at {i}");
        }
        // A window no larger than the block is invalid.
        let mut h = packed.clone();
        h[7] = 16;
        reseal(&mut h);
        assert!(decompress(&h[..], &mut Vec::new(), &dopts(1)).unwrap_err().to_string().contains("window"));
        // The dedup flag with a zero window byte is inconsistent.
        let mut z = packed.clone();
        z[7] = 0;
        reseal(&mut z);
        assert!(decompress(&z[..], &mut Vec::new(), &dopts(1)).unwrap_err().to_string().contains("without a window"));
        // Windows beyond isize::MAX cannot be allocated on any target.
        assert_eq!(window_size(31, 16).is_ok(), isize::MAX as u64 >= 1 << 31);
        assert_eq!(usable(u64::MAX), isize::MAX as u64);
        // A header advertising a window this platform cannot represent is
        // refused cleanly (on 64-bit targets 2^36 is representable).
        assert_eq!(window_size(36, 16).is_ok(), usize::BITS > 36);
        assert!(window_size(37, 16).is_err());
        // The window counts against the memory limit.
        let small = DecodeOptions { threads: 1, memory: 1 << 20 };
        let err = decompress(&packed[..], &mut Vec::new(), &small).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(decompress(&b""[..], &mut Vec::new(), &dopts(1)).is_err());
        assert!(decompress(&b"not compressed"[..], &mut Vec::new(), &dopts(1)).is_err());

        let data: Vec<u8> = (0..5000u32).map(|i| (i * i % 251) as u8).collect();
        for o in ORDERS {
            let mut packed = Vec::new();
            compress(&data[..], &mut packed, &opts(o, 24, 1)).unwrap();
            for cut in 0..packed.len() {
                assert!(decompress(&packed[..cut], &mut Vec::new(), &dopts(1)).is_err(), "{o:?} cut {cut}");
            }
            let mut g = packed.clone();
            g.push(0);
            assert!(decompress(&g[..], &mut Vec::new(), &dopts(1)).is_err());
            // CM allocates about 156 MB per block; sample its flips more sparsely.
            let step = if o == Order::Mix { 7 } else { 1 };
            for i in (0..packed.len()).step_by(step) {
                for bit in [0, 3, 7] {
                    let mut c = packed.clone();
                    c[i] ^= 1 << bit;
                    assert!(
                        decompress(&c[..], &mut Vec::new(), &dopts(1)).is_err(),
                        "{o:?}: flip byte {i} bit {bit} undetected"
                    );
                }
            }
        }
    }
}
