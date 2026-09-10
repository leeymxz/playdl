//! What does the scheduler's tick loop cost when nothing is arriving?
//!
//! `run_transfer_into` wakes on a fixed interval (the CLI passes 20 ms = 50 Hz)
//! for the entire duration of a transfer, and each wake runs `sched.tick()` over
//! every connection plus the watchdog arithmetic. During a fast transfer that is
//! irrelevant â€?the loop is doing real work between wakes. It stops being
//! irrelevant on a server: a slow or stalled transfer, or a long-lived queue with
//! several idle jobs, pays the same 50 Hz forever, and an otherwise-idle box
//! never reaches a deep sleep state.
//!
//! This measures the CPU cost of the loop itself: a transfer from a rate-capped
//! origin slow enough that the tick loop, not the network, dominates the CPU
//! time. The number to watch is CPU-seconds per wall-second.

use pdl_net::origin::OriginSet;
use std::sync::Arc;

/// CPU time (user + system) consumed by this process, in seconds.
#[cfg(unix)]
fn cpu_seconds() -> f64 {
    #[repr(C)]
    #[derive(Default)]
    struct Tv {
        sec: i64,
        usec: i64,
    }
    #[repr(C)]
    #[derive(Default)]
    struct Ru {
        utime: Tv,
        stime: Tv,
        rest: [i64; 14],
    }
    unsafe extern "C" {
        fn getrusage(who: i32, usage: *mut Ru) -> i32;
    }
    let mut ru = Ru::default();
    if unsafe { getrusage(0, &mut ru as *mut Ru) } != 0 {
        return 0.0;
    }
    ru.utime.sec as f64
        + ru.utime.usec as f64 / 1e6
        + ru.stime.sec as f64
        + ru.stime.usec as f64 / 1e6
}

/// CPU time (user + system) consumed by this process, in seconds.
///
/// `GetProcessTimes` reports kernel and user time as FILETIME â€?100 ns units.
/// Declared directly rather than pulling in a crate for one call, matching the
/// unix `getrusage` block above.
#[cfg(windows)]
fn cpu_seconds() -> f64 {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn GetProcessTimes(
            process: isize,
            creation: *mut u64,
            exit: *mut u64,
            kernel: *mut u64,
            user: *mut u64,
        ) -> i32;
    }
    let (mut creation, mut exit, mut kernel, mut user) = (0u64, 0u64, 0u64, 0u64);
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    if ok == 0 {
        return 0.0;
    }
    (kernel + user) as f64 / 1e7
}

/// Fallback: no per-process CPU clock we know how to read; the measurement
/// degrades to zeros rather than failing the build.
#[cfg(not(any(unix, windows)))]
fn cpu_seconds() -> f64 {
    0.0
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    println!("tick_ms,rate_kbps,wall_s,cpu_s,cpu_per_wall_pct,wakes_est");
    // A deliberately slow origin: 256 KiB/s over 2 MiB is ~8 s of wall clock
    // during which almost nothing arrives, so the loop's own cost dominates.
    let size = 2 * 1024 * 1024u64;
    for tick_ms in [20u64, 50, 100, 250] {
        let net = Arc::new(OriginSet::new());
        let (port, _ctl) = net.spawn(size, 256 * 1024);
        let t = pdl_net::Target::direct("127.0.0.1", port, "/obj");
        let sched = pdl_core::Scheduler::new(
            size,
            vec![pdl_core::Source {
                gamma_est: 256.0 * 1024.0,
                delta_est: 0.005,
                ..Default::default()
            }],
            &[2],
        );
        let sink = Arc::new(pdl_net::SparseSink::discarding());
        let c0 = cpu_seconds();
        let w0 = std::time::Instant::now();
        pdl_net::run_transfer_into(
            net.clone(),
            vec![t],
            &[2],
            size,
            sink,
            sched,
            tick_ms,
            &mut |_: &pdl_core::Scheduler, _: u64| {},
            // Unshaped: this harness measures the cost of the tick loop itself,
            // so a rate cap would add sleeps and confound exactly the CPU-time
            // reading it exists to take.
            pdl_net::polite::Pace::unlimited(),
        )
        .await
        .expect("transfer must complete");
        let wall = w0.elapsed().as_secs_f64();
        let cpu = cpu_seconds() - c0;
        println!(
            "{tick_ms},256,{wall:.2},{cpu:.3},{:.1},{:.0}",
            100.0 * cpu / wall.max(1e-9),
            wall * 1000.0 / tick_ms as f64
        );
    }
}
