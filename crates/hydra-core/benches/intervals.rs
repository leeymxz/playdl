//! Interval-set benchmarks.
//!
//! `IntervalSet` is touched once per completed range, not once per byte, so it
//! is not expected to be a bottleneck. It is measured anyway because the
//! scheduler's coverage audit sums over it, and a resumed transfer with a
//! fragmented `.part` file can build a set with thousands of entries â€?the case
//! where an O(n) insert becomes O(n^2) over the transfer.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use pdl_core::intervals::{IntervalSet, Range};
use std::hint::black_box;

fn bench_insert(c: &mut Criterion) {
    let mut g = c.benchmark_group("intervals");
    for n in [64usize, 1024, 8192] {
        // Insert every other range so nothing coalesces and the set grows to n
        // entries: the worst case the structure actually sees.
        g.bench_with_input(BenchmarkId::new("insert_disjoint", n), &n, |b, &n| {
            b.iter(|| {
                let mut s = IntervalSet::new();
                for i in 0..n {
                    let lo = (i as u64) * 2048;
                    s.insert(Range::new(lo, lo + 1024));
                }
                black_box(s.total())
            })
        });
        // Then fill the gaps, forcing a coalesce on every insert.
        g.bench_with_input(BenchmarkId::new("insert_coalescing", n), &n, |b, &n| {
            b.iter(|| {
                let mut s = IntervalSet::new();
                for i in 0..n {
                    let lo = (i as u64) * 2048;
                    s.insert(Range::new(lo, lo + 1024));
                }
                for i in 0..n {
                    let lo = (i as u64) * 2048 + 1024;
                    s.insert(Range::new(lo, lo + 1024));
                }
                black_box(s.total())
            })
        });
    }
    g.finish();
}

criterion_group!(benches, bench_insert);
criterion_main!(benches);
