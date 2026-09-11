// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Deciding how many connections each source gets, before any of them exist.
//!
//! # Why this is a separate decision from scheduling
//!
//! [`crate::sched`] partitions BYTES across connections that already exist, and
//! it does so from measurement. This module answers the question one level up
//! and one moment earlier: given a list of sources and a socket budget, how many
//! connections should each source be given in the first place? Nothing has been
//! measured yet, so the only inputs are the budget, the etiquette ceilings, and
//! whatever the publisher said about their mirrors.
//!
//! It lives in the I/O-free core rather than in the transport because it is
//! pure arithmetic over a policy, it has to be identical under the simulator and
//! under real HTTP, and getting it wrong is invisible at runtime — an allocation
//! that quietly exceeds a stated ceiling looks exactly like one that does not.
//!
//! # What a mirror list adds to the problem
//!
//! Two things a bare URL list does not have:
//!
//! * **A ranking.** Metalink `priority` (RFC 5854) and `preference` (3.0) say
//!   which mirrors the publisher expects to serve well. Splitting evenly ignores
//!   it; splitting only by it concentrates the object on one host and throws
//!   away the redundancy that made the list worth having. [`allocate`] takes it
//!   as a proportional weight, so rank 1 gets more than rank 4 and rank 4 still
//!   gets a connection.
//! * **Per-mirror ceilings.** Metalink 3.0 `maxconnections` is an operator of a
//!   volunteer machine stating a limit for their own host. It binds tighter than
//!   the client's own per-host default and must never be rounded up past.
//!
//! # More sources than sockets is the normal case
//!
//! A distribution image's mirror list names fifteen to twenty hosts; politeness
//! and physics together justify perhaps four connections. So most of the list is
//! not allocated at all — it is a reserve bench, drawn on by
//! [`crate::sched::Scheduler::replace_source`] when a source dies. [`allocate`]
//! therefore returns zero for the surplus rather than shaving everyone to
//! fractional shares, and [`reserves`] names who is on the bench.

/// What is known about one candidate source before the transfer starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourcePlan {
    /// RFC 5854 direction: 1 is best. Use [`crate::sched::NO_PRIORITY`] for
    /// "unranked", which makes every source weigh the same.
    pub priority: u32,
    /// A ceiling stated by the source itself (Metalink `maxconnections`).
    ///
    /// `None` means the source stated none, in which case the client's own
    /// per-host ceiling applies. A stated value NARROWS that ceiling and never
    /// widens it: a mirror operator asking for at most one connection is making
    /// a request about their machine, while a mirror claiming it can take
    /// sixty-four is not entitled to override the user's politeness setting.
    pub max_connections: Option<usize>,
}

impl Default for SourcePlan {
    fn default() -> Self {
        SourcePlan {
            priority: crate::sched::NO_PRIORITY,
            max_connections: None,
        }
    }
}

impl SourcePlan {
    pub fn ranked(priority: u32) -> Self {
        SourcePlan {
            priority,
            ..Default::default()
        }
    }

    /// This source's ceiling, given the client's per-host limit.
    fn cap(&self, per_host: usize) -> usize {
        let client = per_host.max(1);
        match self.max_connections {
            Some(n) => n.clamp(1, client),
            None => client,
        }
    }

    fn weight(&self) -> f64 {
        1.0 / (self.priority.max(1) as f64)
    }
}

/// Split `requested` connections across `sources`, honouring every ceiling.
///
/// Returns one entry per source, **in input order** — a caller's own per-source
/// bookkeeping (targets, hostnames, progress rows) is index-aligned with this,
/// and re-ordering the result to put the best mirror first would silently
/// scramble it.
///
/// The three ceilings, all enforced:
///
/// * `requested` — what the caller asked for, usually `-x` or a measured
///   concurrency.
/// * `total` — the aggregate socket ceiling. Eight connections across two
///   mirrors is still eight sockets, which is the number a server operator
///   actually feels.
/// * `per_host`, narrowed by any [`SourcePlan::max_connections`].
///
/// # The allocation rule
///
/// Every source that gets anything gets at least one, best-ranked first; the
/// remainder is distributed by the divisor method — repeatedly give the next
/// connection to whichever source maximises `weight / (held + 1)`. That is the
/// standard proportional-apportionment rule, and it is used here for the
/// property that makes it standard: it never leaves a source with a share that
/// another source's ranking cannot justify, and it terminates in exactly
/// `budget` steps with no rounding residue to strand.
///
/// Ties break on `(priority, index)` so two runs against the same mirror list
/// allocate identically. A download that opens different mirrors on every
/// attempt cannot be debugged from its logs.
pub fn allocate(
    sources: &[SourcePlan],
    requested: usize,
    per_host: usize,
    total: usize,
) -> Vec<usize> {
    let n = sources.len();
    let mut out = vec![0usize; n];
    if n == 0 {
        return out;
    }
    let budget = requested.max(1).min(total.max(1));
    let caps: Vec<usize> = sources.iter().map(|s| s.cap(per_host)).collect();

    // Best-ranked first, stable on index. This order decides WHO participates
    // when there are more sources than sockets; it does not decide the shape of
    // the output, which stays in input order.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (sources[i].priority, i));

    // One each, to as many sources as the budget can seat.
    let seated = n.min(budget);
    for &i in order.iter().take(seated) {
        out[i] = 1;
    }
    let mut left = budget - seated;

    // Divisor method over the seated sources.
    while left > 0 {
        let mut best: Option<(usize, f64)> = None;
        for &i in order.iter().take(seated) {
            if out[i] >= caps[i] {
                continue;
            }
            let score = sources[i].weight() / (out[i] + 1) as f64;
            // Strictly greater keeps the `order` tie-break: the first source in
            // rank order wins an exact tie.
            if best.map(|(_, b)| score > b).unwrap_or(true) {
                best = Some((i, score));
            }
        }
        let Some((i, _)) = best else {
            // Every seated source is at its ceiling. The remaining budget is not
            // reassigned to unseated sources: seating another host to spend
            // sockets the ranked ones were not permitted would be a politeness
            // ceiling defeated by arithmetic.
            break;
        };
        out[i] += 1;
        left -= 1;
    }
    out
}

/// The sources [`allocate`] gave no connections to, best-ranked first.
///
/// These are not rejected sources — they are the bench. When a source fails,
/// [`crate::sched::Scheduler::replace_source`] substitutes one of these in
/// place, which is what makes a nineteen-mirror list worth more than a
/// four-mirror one at four connections.
pub fn reserves(sources: &[SourcePlan], allocation: &[usize]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..sources.len())
        .filter(|&i| allocation.get(i).copied().unwrap_or(0) == 0)
        .collect();
    idx.sort_by_key(|&i| (sources[i].priority, i));
    idx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sched::NO_PRIORITY;

    fn flat(n: usize) -> Vec<SourcePlan> {
        vec![SourcePlan::default(); n]
    }

    #[test]
    fn an_unranked_list_splits_evenly_and_the_remainder_leads() {
        // Matches the behaviour that existed before rankings did: `5` over `2` is
        // `3, 2`, not `4, 1`.
        assert_eq!(allocate(&flat(2), 5, 8, 16), vec![3, 2]);
        assert_eq!(allocate(&flat(1), 4, 8, 16), vec![4]);
        assert_eq!(allocate(&flat(4), 4, 8, 16), vec![1, 1, 1, 1]);
    }

    #[test]
    fn a_ranking_shifts_share_without_starving_the_rest() {
        // Splitting only by rank would concentrate the object on one host and
        // throw away the redundancy the list exists to provide.
        let s = vec![SourcePlan::ranked(1), SourcePlan::ranked(4)];
        let got = allocate(&s, 6, 8, 16);
        assert_eq!(got.iter().sum::<usize>(), 6);
        assert!(got[0] > got[1], "rank 1 must lead rank 4: {got:?}");
        assert!(got[1] >= 1, "rank 4 must not be starved: {got:?}");
    }

    #[test]
    fn the_output_is_in_input_order_not_rank_order() {
        // A caller's targets, hostnames and progress rows are index-aligned with
        // this. Re-ordering to put the best mirror first would scramble them
        // silently — every row would describe a different host than it fetches
        // from.
        let s = vec![SourcePlan::ranked(9), SourcePlan::ranked(1)];
        let got = allocate(&s, 4, 8, 16);
        assert!(got[1] > got[0], "the better mirror is at index 1: {got:?}");
    }

    #[test]
    fn the_aggregate_ceiling_is_never_multiplied_by_the_mirror_count() {
        // Eight connections over two mirrors is still eight sockets, which is
        // what an operator feels.
        for n in 1..8usize {
            let got = allocate(&flat(n), 8, 8, 2);
            assert_eq!(got.iter().sum::<usize>(), 2, "n={n} {got:?}");
        }
    }

    #[test]
    fn a_mirrors_own_stated_ceiling_narrows_but_never_widens_the_clients() {
        // `maxconnections="1"` is an operator of a volunteer machine stating a
        // limit for their own host, and it must not be rounded up past.
        let s = vec![
            SourcePlan {
                priority: 1,
                max_connections: Some(1),
            },
            SourcePlan::ranked(2),
        ];
        let got = allocate(&s, 6, 4, 16);
        assert_eq!(got[0], 1, "the stated ceiling binds: {got:?}");
        assert!(got[1] > 1);

        // A mirror claiming it can take sixty-four does not get to override the
        // user's own politeness setting.
        let greedy = vec![SourcePlan {
            priority: 1,
            max_connections: Some(64),
        }];
        assert_eq!(allocate(&greedy, 16, 4, 16), vec![4]);
    }

    #[test]
    fn surplus_budget_is_dropped_rather_than_spent_on_an_unseated_host() {
        // Every seated source at its ceiling with budget left over. Seating
        // another host to spend it would defeat the per-host ceiling by
        // arithmetic — the aggregate would be honoured and the intent would not.
        let s = vec![
            SourcePlan {
                priority: 1,
                max_connections: Some(1),
            },
            SourcePlan {
                priority: 2,
                max_connections: Some(1),
            },
            SourcePlan::ranked(3),
        ];
        let got = allocate(&s, 2, 4, 16);
        assert_eq!(got, vec![1, 1, 0]);
        assert_eq!(got.iter().sum::<usize>(), 2);
    }

    #[test]
    fn more_mirrors_than_sockets_leaves_a_reserve_bench_in_rank_order() {
        // The normal case for a real mirror list: nineteen hosts, four sockets.
        let s: Vec<SourcePlan> = (0..19).map(|i| SourcePlan::ranked(19 - i as u32)).collect();
        let got = allocate(&s, 4, 4, 16);
        assert_eq!(got.iter().sum::<usize>(), 4);
        assert_eq!(got.iter().filter(|&&n| n > 0).count(), 4);
        // The four best-ranked hosts are the ones seated: priorities 1..4, which
        // are the LAST four entries by construction.
        assert!(got[15..].iter().all(|&n| n > 0), "{got:?}");

        let bench = reserves(&s, &got);
        assert_eq!(bench.len(), 15);
        // Best-ranked reserve first: it is the next one substituted in.
        assert_eq!(s[bench[0]].priority, 5);
        assert!(bench.iter().all(|&i| got[i] == 0));
    }

    #[test]
    fn allocation_is_deterministic_across_runs() {
        // A download that opens different mirrors on every attempt cannot be
        // debugged from its logs.
        let s = vec![
            SourcePlan::ranked(3),
            SourcePlan::ranked(3),
            SourcePlan::ranked(3),
        ];
        let first = allocate(&s, 7, 4, 16);
        for _ in 0..50 {
            assert_eq!(allocate(&s, 7, 4, 16), first);
        }
        // An exact tie goes to the earlier index.
        assert!(first[0] >= first[1] && first[1] >= first[2], "{first:?}");
    }

    #[test]
    fn degenerate_inputs_do_not_panic_or_over_allocate() {
        assert!(allocate(&[], 4, 4, 16).is_empty());
        // Zero is not a socket count anyone can act on; one is the floor.
        assert_eq!(allocate(&flat(1), 0, 4, 16).iter().sum::<usize>(), 1);
        assert_eq!(allocate(&flat(3), 4, 0, 16).iter().sum::<usize>(), 3);
        assert_eq!(allocate(&flat(3), 4, 4, 0).iter().sum::<usize>(), 1);
        // An unranked source and NO_PRIORITY are the same thing.
        assert_eq!(
            allocate(&[SourcePlan::ranked(NO_PRIORITY)], 3, 4, 16),
            allocate(&flat(1), 3, 4, 16)
        );
    }
}
