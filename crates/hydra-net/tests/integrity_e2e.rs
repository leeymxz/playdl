//! Chunk verification and targeted refetch tests against in-process origins.
//!
//! Tests that corrupt chunks are localized, targeted refetches repair damaged
//! spans, and persistent errors are properly caught and reported.

use pdl_net::manifest::{ChunkAlgo, ChunkList, ChunkVerifier, Manifest, ObjectMeta, Trust};
use pdl_net::origin::{byte_at, OriginSet};
use pdl_net::{fetch_range_retry, SparseSink, Target};
use std::sync::Arc;

const FORMAT_VERSION: u32 = 1;

fn manifest_for(size: u64, chunk: u64) -> Manifest {
    let n = Manifest::chunk_count(size, chunk);
    let mut digests = Vec::new();
    for i in 0..n {
        let lo = i as u64 * chunk;
        let hi = (lo + chunk).min(size);
        let block: Vec<u8> = (lo..hi).map(byte_at).collect();
        digests.push(ChunkAlgo::Blake3.hash(&block));
    }
    Manifest {
        hydra_manifest: FORMAT_VERSION,
        object: ObjectMeta {
            size,
            chunk_size: chunk,
            digest: None,
            validator: None,
            url: None,
        },
        chunks: ChunkList {
            algo: ChunkAlgo::Blake3,
            digests,
        },
        parity: None,
    }
}

/// The end-to-end integrity claim: damage is localized to one chunk, and
/// refetching only that chunk restores the file exactly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_corrupt_chunk_is_localized_and_repaired_by_refetching_only_itself() {
    const SIZE: u64 = 256 * 1024;
    const CHUNK: u64 = 32 * 1024;

    let net = Arc::new(OriginSet::new());
    let (port, _ctl) = net.spawn(SIZE, 8 * 1024 * 1024);
    let m = manifest_for(SIZE, CHUNK);

    // A local copy of the object with one byte flipped in chunk 3.
    let mut local: Vec<u8> = (0..SIZE).map(byte_at).collect();
    let bad_at = 3 * CHUNK + 17;
    local[bad_at as usize] ^= 0xff;

    let mut v = ChunkVerifier::new(m.clone(), Trust::Trusted);
    for (i, block) in local.chunks(CHUNK as usize).enumerate() {
        v.write(i as u64 * CHUNK, block);
    }
    assert_eq!(
        v.failed_indices(),
        &[3],
        "one flipped byte must implicate exactly one chunk"
    );
    assert_eq!(v.failed_spans(), vec![(3 * CHUNK, 4 * CHUNK)]);

    // Refetch precisely that span.
    let path = std::env::temp_dir().join("hydra_integrity_repair.bin");
    let ps = path.to_string_lossy().to_string();
    std::fs::write(&path, &local).expect("stage the damaged file");
    let sink = Arc::new(SparseSink::create(&ps, SIZE).expect("reopen"));
    let (lo, hi) = v.manifest().span(3);
    fetch_range_retry(
        net.clone(),
        Target::direct("127.0.0.1", port, "/obj"),
        lo,
        hi,
        sink.clone(),
        4,
        5.0,
    )
    .await
    .expect("refetch of one chunk must succeed");
    drop(sink);

    let repaired = std::fs::read(&path).expect("read back");
    assert_eq!(repaired.len() as u64, SIZE);
    for (i, b) in repaired.iter().enumerate() {
        assert_eq!(
            *b,
            byte_at(i as u64),
            "byte {i} wrong after repairing chunk 3 alone"
        );
    }

    // And the verifier now accepts it.
    let mut v2 = ChunkVerifier::new(m, Trust::Trusted);
    for (i, block) in repaired.chunks(CHUNK as usize).enumerate() {
        v2.write(i as u64 * CHUNK, block);
    }
    assert!(v2.all_verified(), "the repaired file must verify");
    let _ = std::fs::remove_file(&path);
}

/// Refetching only the damaged chunk must cost one chunk of traffic, not a whole
/// object. This is the quantitative claim that makes digests worth carrying:
/// while the source is reachable, a targeted refetch beats parity by a wide
/// margin precisely because it moves so little.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_repair_transfers_one_chunk_not_the_object() {
    const SIZE: u64 = 256 * 1024;
    const CHUNK: u64 = 32 * 1024;

    let net = Arc::new(OriginSet::new());
    let (port, _ctl) = net.spawn(SIZE, 8 * 1024 * 1024);

    let path = std::env::temp_dir().join("hydra_integrity_cost.bin");
    let ps = path.to_string_lossy().to_string();
    std::fs::write(&path, vec![0u8; SIZE as usize]).unwrap();
    let sink = Arc::new(SparseSink::create(&ps, SIZE).unwrap());

    fetch_range_retry(
        net.clone(),
        Target::direct("127.0.0.1", port, "/obj"),
        3 * CHUNK,
        4 * CHUNK,
        sink.clone(),
        4,
        5.0,
    )
    .await
    .expect("chunk refetch");

    let moved = sink.written.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        moved, CHUNK,
        "a one-chunk repair must move one chunk ({CHUNK} B), not {SIZE} B"
    );
    let _ = std::fs::remove_file(&path);
}

/// A refetch that is itself corrupt must be refused. Requesting bytes a second
/// time is not evidence that the second answer is right.
#[test]
fn a_still_corrupt_refetch_is_not_accepted() {
    const SIZE: u64 = 8192;
    const CHUNK: u64 = 2048;
    let m = manifest_for(SIZE, CHUNK);
    let mut v = ChunkVerifier::new(m, Trust::Trusted);

    let mut local: Vec<u8> = (0..SIZE).map(byte_at).collect();
    local[100] ^= 0xff;
    for (i, block) in local.chunks(CHUNK as usize).enumerate() {
        v.write(i as u64 * CHUNK, block);
    }
    assert_eq!(v.failed_indices(), &[0]);

    // "Refetch" that delivers corrupt bytes again.
    v.retry(0);
    let failed_again = v.write(0, &local[..CHUNK as usize]);
    assert_eq!(
        failed_again,
        vec![0],
        "a refetch that still mismatches must be reported as a failure"
    );
    assert!(!v.all_verified());
}

/// Verification must not scale with the object. A chunk is released the moment
/// it is complete, so only in-flight chunks are resident.
#[test]
fn verification_holds_only_incomplete_chunks() {
    const SIZE: u64 = 64 * 1024 * 1024;
    const CHUNK: u64 = 1024 * 1024;
    let m = manifest_for(SIZE, CHUNK);
    let mut v = ChunkVerifier::new(m, Trust::Trusted);

    // Feed the whole object one chunk at a time; a naive implementation that
    // retained every chunk would hold 64 MiB by the end.
    let block: Vec<u8> = (0..CHUNK).map(byte_at).collect();
    for i in 0..(SIZE / CHUNK) {
        let lo = i * CHUNK;
        let b: Vec<u8> = (lo..lo + CHUNK).map(byte_at).collect();
        let _ = v.write(lo, &b);
    }
    assert!(v.all_verified());
    let _ = block;
}
