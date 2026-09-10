//! `SparseSink::written` must be exact under concurrent writes.
//!
//! The counter was implemented as `written.store(written.load() + n)` â€?two
//! separate atomic operations, which is not an atomic read-modify-write. Two
//! connections that load the same value both store their own sum and one
//! write's bytes disappear from the count. Every connection calls `write_at`
//! concurrently, so the bug is live on every multi-connection transfer; it is
//! invisible with one connection, which is why it survived.
//!
//! `written` feeds the progress display and the completion accounting, so an
//! undercount makes a finished transfer look short.
//!
//! This test asserts the EFFECT (an exact total) rather than that the code
//! merely runs, which is the only formulation that can catch a lost update: the
//! racy version returns a plausible number, just a wrong one.

use pdl_net::SparseSink;
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[test]
fn concurrent_writes_are_counted_exactly() {
    const THREADS: usize = 8;
    const WRITES: usize = 4000;
    const CHUNK: usize = 512;

    // A discarding sink: this measures the counter, not the filesystem.
    let sink = Arc::new(SparseSink::discarding());
    let block = vec![0x7eu8; CHUNK];

    let mut handles = Vec::new();
    for t in 0..THREADS {
        let sink = Arc::clone(&sink);
        let block = block.clone();
        handles.push(std::thread::spawn(move || {
            // Disjoint offsets, as the scheduler's coverage invariant guarantees.
            let base = (t * WRITES * CHUNK) as u64;
            for i in 0..WRITES {
                sink.write_at(base + (i * CHUNK) as u64, &block)
                    .expect("discarding sink cannot fail");
            }
        }));
    }
    for h in handles {
        h.join().expect("writer thread panicked");
    }

    let want = (THREADS * WRITES * CHUNK) as u64;
    let got = sink.written.load(Ordering::Relaxed);
    assert_eq!(
        got,
        want,
        "counted {got} of {want} bytes: {} lost to a non-atomic read-modify-write",
        want - got
    );
}
