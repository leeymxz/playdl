//! Does the streaming digest survive a normal multi-connection transfer?
//!
//! `stream_digest`'s module docs claim "ordinary transfers do not come close to
//! the cap ... the buffer holds at most a few in-flight spans." This probe
//! reproduces the layout the scheduler actually produces â€?n connections each
//! assigned a CONTIGUOUS span, all advancing at similar rates â€?and reports
//! whether the digest survives.
use pdl_net::stream_digest::{StreamDigest, DEFAULT_REORDER_CAP};

fn main() {
    println!("size_mb,conns,peak_pending_mb,abandoned");
    for size_mb in [16u64, 64, 256] {
        for conns in [1usize, 2, 4, 8] {
            let size = size_mb * 1024 * 1024;
            let read = 64 * 1024u64;
            let mut sd = StreamDigest::new(DEFAULT_REORDER_CAP);
            let block = vec![0u8; read as usize];
            let span = size / conns as u64;
            let steps = span / read;
            for s in 0..steps {
                for c in 0..conns as u64 {
                    let off = c * span + s * read;
                    if off < size {
                        sd.write(off, &block);
                    }
                }
            }
            // Peak pending is bounded by the cap; report what the layout demands.
            let demanded = (conns as u64 - 1) * span;
            println!(
                "{size_mb},{conns},{:.1},{}",
                demanded as f64 / 1048576.0,
                sd.is_abandoned()
            );
        }
    }
}
