//! Count heap allocations on the byte path, per MiB transferred.
//!
//! `hydra bench --which memprofile` reports peak RSS, which is the right measure
//! for the flat-memory claim but says nothing about allocation CHURN: a loop that
//! allocates and frees a buffer per read has a flat footprint and still burns
//! measurable time in the allocator, and on a multi-threaded transfer it
//! contends. This harness answers the other question — how many allocations does
//! moving a megabyte cost — by installing a counting global allocator and
//! driving the real chunked decoder over a real socket.
//!
//! Chunked framing is the mode measured because it is the one with per-token
//! work; a plain 206 body is a straight `read`-to-`write_at` loop with no
//! framing at all.
//!
//! # Reading the numbers
//!
//! The counter is process-wide, so it also sees the in-process test ORIGIN,
//! which allocates a `format!("{n:x}\r\n")` per chunk it emits. That is the
//! harness, not hydra. The `origin_only` row isolates it: run the same transfer
//! with the origin serving an unframed body and subtract. What matters for the
//! client is the DELTA between chunk sizes after that subtraction.

use pdl_net::origin::OriginSet;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// Histogram of allocation sizes by power-of-two bucket.
static BUCKETS: [AtomicU64; 24] = [const { AtomicU64::new(0) }; 24];

/// Set to capture a backtrace on the next allocation of exactly this size.
pub static TRACE_SIZE: AtomicUsize = AtomicUsize::new(0);
static TRACED: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        let t = TRACE_SIZE.load(Ordering::Relaxed);
        if t != 0 && l.size() == t && TRACED.fetch_add(1, Ordering::Relaxed) < 3 {
            // Printing allocates; the guard above bounds the recursion depth.
            eprintln!(
                "--- alloc of {} bytes ---\n{}",
                t,
                std::backtrace::Backtrace::force_capture()
            );
        }
        BYTES.fetch_add(l.size() as u64, Ordering::Relaxed);
        let b = (usize::BITS - l.size().leading_zeros()).min(23) as usize;
        BUCKETS[b].fetch_add(1, Ordering::Relaxed);
        let live = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
        PEAK.fetch_max(live, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Counting = Counting;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    println!("size_mib,chunk_kib,allocs,alloc_bytes,peak_live_kib,allocs_per_mib,bytes_per_alloc");
    // chunk == 0 means an unframed body: the control that shows what the
    // transfer costs with no framing work at all on either side.
    for (size_mib, chunk) in [
        (16u64, 0usize),
        (16, 1024),
        (16, 16 * 1024),
        (64, 0),
        (64, 1024),
    ] {
        let size = size_mib * 1024 * 1024;
        let net = Arc::new(OriginSet::new());
        // Rate high enough that pacing never binds: this measures allocator
        // traffic per byte, not the origin's token bucket.
        let (port, ctl) = net.spawn(size, 8 * 1024 * 1024 * 1024);
        ctl.chunked.store(chunk as u64, Ordering::Relaxed);
        let t = pdl_net::Target::direct("127.0.0.1", port, "/obj");
        // Discarding sink isolates decode-path allocation from the filesystem.
        let sink = Arc::new(pdl_net::SparseSink::discarding());

        // Reset after setup so the counts describe the transfer, not the server.
        ALLOCS.store(0, Ordering::Relaxed);
        BYTES.store(0, Ordering::Relaxed);
        PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);

        pdl_net::fetch_range_retry(net.clone(), t, 0, size, sink.clone(), 2, 120.0)
            .await
            .expect("chunked transfer must complete");

        for (i, b) in BUCKETS.iter().enumerate() {
            let v = b.swap(0, Ordering::Relaxed);
            if v > 100 {
                eprintln!(
                    "  size ~2^{i} ({} B): {v} allocs",
                    1usize << i.saturating_sub(1)
                );
            }
        }
        let allocs = ALLOCS.load(Ordering::Relaxed).max(1);
        let abytes = BYTES.load(Ordering::Relaxed);
        let peak = PEAK.load(Ordering::Relaxed);
        let mib = size_mib as f64;
        println!(
            "{size_mib},{},{allocs},{abytes},{},{:.1},{}",
            chunk / 1024,
            peak / 1024,
            allocs as f64 / mib,
            abytes / allocs
        );
    }
}
