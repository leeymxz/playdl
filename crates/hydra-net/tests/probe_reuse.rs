//! The size probe's connection must survive into the transfer.
//!
//! Every run begins with a HEAD to learn the object's size and whether the origin
//! supports ranges. That is the client's first contact with the host, so its TCP
//! handshake — and on HTTPS its TLS handshake — is the most expensive one of the
//! whole run and the one most worth keeping. Before the pool was shared through the
//! connector, the probe dialled, asked the server to close, and the transfer that
//! began milliseconds later dialled the same host again.
//!
//! Measured on a live TLS path: 1.6-2.0 s elapsed before the transfer's first byte,
//! against a transfer body that took 3.7-5.5 s. The setup, not the transfer, was
//! the main performance bottleneck.
//!
//! This is measured at the ORIGIN, by counting accepted connections, because the
//! client cannot see the difference: a redialled transfer delivers exactly the same
//! bytes as a reused one.
//!
//! # Harness fidelity limit
//!
//! The in-process origin does NOT honour a client's `Connection: close` — it answers
//! keep-alive whenever the control flag is set, regardless of what was asked. So
//! flipping the probe's request disposition does not ablate this test; the mechanism
//! it actually pins is the pool shared through `Connector::pool`. Returning `None`
//! there makes the origin accept 2 connections instead of 1, which is the assertion
//! below. A real server would also close on request, making the effect strictly
//! larger in production than it is here.

use pdl_core::{Scheduler, Source};
use pdl_net::origin::OriginSet;
use pdl_net::{run_transfer, Connector, Target};
use std::sync::atomic::Ordering;

fn tgt(port: u16) -> Target {
    Target::direct("127.0.0.1", port, "/obj")
}

/// A connector that keeps one pool across every request, as the real TLS connector
/// does. Without this the test measures nothing: the in-process test connector
/// returns `None` from `pool()`, so probe and transfer each build their own.
struct PooledConnector {
    inner: OriginSet,
    pool: pdl_net::pool::SharedPool<<OriginSet as Connector>::Stream>,
}

impl Connector for PooledConnector {
    type Stream = <OriginSet as Connector>::Stream;

    fn connect<'a>(
        &'a self,
        t: &'a Target,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<Self::Stream>> + Send + 'a>,
    > {
        self.inner.connect(t)
    }

    fn pool(&self) -> Option<pdl_net::pool::SharedPool<Self::Stream>> {
        Some(self.pool.clone())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_probes_connection_is_reused_by_the_transfer() {
    const SIZE: u64 = 2 * 1024 * 1024;
    let net = OriginSet::new();
    let (port, ctl) = net.spawn(SIZE, 8 * 1024 * 1024);
    ctl.keep_alive.store(true, Ordering::Relaxed);

    let c = std::sync::Arc::new(PooledConnector {
        inner: net,
        pool: std::sync::Arc::new(pdl_net::pool::ConnPool::new()),
    });

    // The probe: exactly what the CLI does before every transfer.
    let p = pdl_net::http::probe(c.as_ref(), &tgt(port))
        .await
        .expect("probe must succeed");
    assert_eq!(p.size, SIZE, "probe must read the object's size");
    let after_probe = ctl.connections.load(Ordering::Relaxed);
    assert_eq!(after_probe, 1, "the probe opens exactly one connection");

    let out = std::env::temp_dir().join("hydra_probe_reuse.bin");
    let outs = out.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&out);

    let sched = Scheduler::new(
        SIZE,
        vec![Source {
            gamma_est: 2e6,
            delta_est: 0.01,
            ..Default::default()
        }],
        &[1],
    );
    run_transfer(c.clone(), vec![tgt(port)], &[1], SIZE, &outs, sched)
        .await
        .expect("transfer must complete");

    let total = ctl.connections.load(Ordering::Relaxed);
    assert_eq!(
        total, 1,
        "origin accepted {total} connections for a probe plus a single-connection \
         transfer: the probe's connection was thrown away and re-dialled"
    );
    assert_eq!(
        std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0),
        SIZE,
        "the reused connection must still deliver the whole object"
    );
    let _ = std::fs::remove_file(&out);
}
