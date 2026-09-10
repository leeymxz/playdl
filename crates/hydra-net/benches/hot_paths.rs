//! Throughput benchmarks for the paths every transferred byte passes through.
//!
//! Only four things in this project run per-byte or per-read: the chunked-body
//! decoder, the sink write, the streaming digest, and hex encoding of digest
//! output. Everything else runs per-request or per-transfer and is dominated by
//! network latency, so optimizing it would be measuring the wrong thing.
//!
//! `chunked_baseline` below is a faithful transcription of the decoder as it is
//! written in `lib.rs` at the time this harness was added â€?same `windows(2)`
//! CRLF scan, same `drain` after every token. It exists so the "before" number
//! in the comparison table is the shipped algorithm rather than a strawman, and
//! it must not be edited when the real decoder is optimized.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use pdl_net::digest::to_lower_hex;
use pdl_net::stream_digest::StreamDigest;
use std::hint::black_box;

const READ_BUF: usize = 64 * 1024;

/// Build a `Transfer-Encoding: chunked` body carrying `total` bytes of payload
/// in chunks of `chunk` bytes.
fn build_chunked_body(total: usize, chunk: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(total + (total / chunk) * 16 + 8);
    let mut left = total;
    let mut fill = 0u8;
    while left > 0 {
        let n = chunk.min(left);
        out.extend_from_slice(format!("{n:x}\r\n").as_bytes());
        out.extend(std::iter::repeat_n(fill, n));
        out.extend_from_slice(b"\r\n");
        fill = fill.wrapping_add(1);
        left -= n;
    }
    out.extend_from_slice(b"0\r\n\r\n");
    out
}

enum St {
    Size,
    Data(u64),
    AfterData,
    Done,
}

/// The decoder exactly as `stream_chunked` writes it today. Sink writes are
/// replaced by a counter so the benchmark measures framing, not the filesystem.
fn chunked_baseline(body: &[u8], read_size: usize) -> u64 {
    let mut buf: Vec<u8> = Vec::new();
    let mut state = St::Size;
    let mut written = 0u64;
    let mut src = 0usize;

    loop {
        loop {
            match &mut state {
                St::Size => {
                    let Some(nl) = buf.windows(2).position(|w| w == b"\r\n") else {
                        break;
                    };
                    let line = String::from_utf8_lossy(&buf[..nl]);
                    let hex = line.split(';').next().unwrap_or("").trim();
                    let size = u64::from_str_radix(hex, 16).expect("valid size line");
                    buf.drain(..nl + 2);
                    state = if size == 0 { St::Done } else { St::Data(size) };
                }
                St::Data(remaining) => {
                    if buf.is_empty() {
                        break;
                    }
                    let take = (*remaining as usize).min(buf.len());
                    written += black_box(&buf[..take]).len() as u64;
                    buf.drain(..take);
                    *remaining -= take as u64;
                    if *remaining == 0 {
                        state = St::AfterData;
                    }
                }
                St::AfterData => {
                    if buf.len() < 2 {
                        break;
                    }
                    buf.drain(..2);
                    state = St::Size;
                }
                St::Done => return written,
            }
        }
        if src >= body.len() {
            return written;
        }
        let n = read_size.min(body.len() - src);
        buf.extend_from_slice(&body[src..src + n]);
        src += n;
    }
}

/// The same state machine driven by `FrameBuf` â€?the shape `stream_chunked` now
/// uses. Kept beside `chunked_baseline` so before and after are measured in one
/// process on one set of inputs, rather than compared across two runs.
fn chunked_optimized(body: &[u8], read_size: usize) -> u64 {
    use pdl_net::framebuf::FrameBuf;
    let mut buf = FrameBuf::new();
    let mut state = St::Size;
    let mut written = 0u64;
    let mut src = 0usize;

    loop {
        loop {
            match &mut state {
                St::Size => {
                    let Some(nl) = buf.find_crlf() else { break };
                    let line = &buf.data()[..nl];
                    let end = line.iter().position(|&b| b == b';').unwrap_or(line.len());
                    let hex = std::str::from_utf8(&line[..end]).expect("ascii").trim();
                    let size = u64::from_str_radix(hex, 16).expect("valid size line");
                    buf.consume(nl + 2);
                    state = if size == 0 { St::Done } else { St::Data(size) };
                }
                St::Data(remaining) => {
                    if buf.is_empty() {
                        break;
                    }
                    let take = (*remaining as usize).min(buf.len());
                    written += black_box(&buf.data()[..take]).len() as u64;
                    buf.consume(take);
                    *remaining -= take as u64;
                    if *remaining == 0 {
                        state = St::AfterData;
                    }
                }
                St::AfterData => {
                    if buf.len() < 2 {
                        break;
                    }
                    buf.consume(2);
                    state = St::Size;
                }
                St::Done => return written,
            }
        }
        if src >= body.len() {
            return written;
        }
        let n = read_size.min(body.len() - src);
        buf.extend(&body[src..src + n]);
        src += n;
    }
}

fn bench_chunked(c: &mut Criterion) {
    let mut g = c.benchmark_group("chunked_decode");
    // 8 MiB of payload is enough that per-read costs dominate startup, and small
    // enough to keep the whole suite under a minute.
    const TOTAL: usize = 8 * 1024 * 1024;
    for chunk in [1024usize, 16 * 1024, 1024 * 1024] {
        let body = build_chunked_body(TOTAL, chunk);
        // Both arms must decode the same payload byte count, or the comparison
        // is between two different amounts of work.
        assert_eq!(
            chunked_baseline(&body, READ_BUF),
            chunked_optimized(&body, READ_BUF),
            "decoders disagree at chunk size {chunk}"
        );
        g.throughput(Throughput::Bytes(TOTAL as u64));
        g.bench_with_input(BenchmarkId::new("baseline", chunk), &body, |b, body| {
            b.iter(|| chunked_baseline(black_box(body), READ_BUF))
        });
        g.bench_with_input(BenchmarkId::new("framebuf", chunk), &body, |b, body| {
            b.iter(|| chunked_optimized(black_box(body), READ_BUF))
        });
    }
    g.finish();
}

fn bench_hex(c: &mut Criterion) {
    let mut g = c.benchmark_group("to_lower_hex");
    // A SHA-256 digest: the size this is actually called with, once per manifest
    // chunk. A manifest for a large object has tens of thousands of these.
    let d32 = [0xa7u8; 32];
    g.throughput(Throughput::Bytes(32));
    g.bench_function("32B", |b| b.iter(|| to_lower_hex(black_box(&d32))));

    // Bulk: what building a manifest for a 40 GiB object at 1 MiB chunks costs.
    let bulk = vec![0x5cu8; 32 * 40_000];
    g.throughput(Throughput::Bytes(bulk.len() as u64));
    g.bench_function("40k_digests", |b| {
        b.iter(|| {
            let mut acc = 0usize;
            for c in black_box(&bulk).as_chunks::<32>().0 {
                acc += to_lower_hex(c).len();
            }
            acc
        })
    });
    g.finish();
}

fn bench_stream_digest(c: &mut Criterion) {
    let mut g = c.benchmark_group("stream_digest");
    const TOTAL: u64 = 16 * 1024 * 1024;
    let block = vec![0x3bu8; READ_BUF];
    g.throughput(Throughput::Bytes(TOTAL));

    // The common case: one connection, arrivals already contiguous.
    g.bench_function("in_order", |b| {
        b.iter(|| {
            let mut sd = StreamDigest::new(8 * 1024 * 1024);
            let mut off = 0u64;
            while off < TOTAL {
                sd.write(off, black_box(&block));
                off += block.len() as u64;
            }
            sd.finish(TOTAL)
        })
    });

    // The real case: four connections interleaving, so most fragments land ahead
    // of the frontier and go through the reorder buffer.
    let n = (TOTAL / READ_BUF as u64) as usize;
    let order: Vec<u64> = (0..n)
        .map(|i| {
            let slot = ((i % 4) * (n / 4) + (i / 4)).min(n - 1);
            (slot * READ_BUF) as u64
        })
        .collect();
    g.bench_function("interleaved_4conn", |b| {
        b.iter(|| {
            let mut sd = StreamDigest::new(8 * 1024 * 1024);
            for &off in black_box(&order) {
                sd.write(off, black_box(&block));
            }
            sd.finish(TOTAL)
        })
    });
    g.finish();
}

fn bench_sink(c: &mut Criterion) {
    let mut g = c.benchmark_group("sink_write_at");
    const TOTAL: u64 = 16 * 1024 * 1024;
    let block = vec![0x11u8; READ_BUF];
    g.throughput(Throughput::Bytes(TOTAL));
    // Discarding sink: isolates the accounting overhead per write from the
    // filesystem, which is what the atomic counter change affects.
    g.bench_function("discarding", |b| {
        b.iter(|| {
            let sink = pdl_net::SparseSink::discarding();
            let mut off = 0u64;
            while off < TOTAL {
                sink.write_at(off, black_box(&block)).expect("discard sink");
                off += block.len() as u64;
            }
            off
        })
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_chunked,
    bench_hex,
    bench_stream_digest,
    bench_sink
);
criterion_main!(benches);
