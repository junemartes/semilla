//! The budgets of the checks in solver steps (design 77, part B).
//!
//! [R77.2-1] Each budget that decides a result counts solver steps: the resource count of the
//! solver (`rlimit` of Z3) for each query, and a fixed count for each step of the machine of the
//! check. The same trees and the same intent files give the same counts on each machine and at
//! each load, so they give the same results. A time limit stays only as a safety stop: a result
//! that it stops has its own cause ([`SAFETY_CAUSE`]), never the cause of the budget.
//!
//! The meter is a value of the thread: a check runs in one thread, and a nested check (a link of
//! two closures) keeps the smaller budget of its outer check.

use std::cell::Cell;
use std::time::{Duration, Instant};

/// The resource limit of one solver query (the most solver steps). A query that reaches it gives
/// "the solver gave no answer (the step limit of one query)".
pub const QUERY_STEPS: u64 = 20_000_000;

/// The solver steps that one step of the machine of the check counts (a statement or a
/// terminator of a run). The machine counts its steps in groups of 256.
pub const MACHINE_STEP_COST: u64 = 16;

/// The safety time limit of one solver query. It never decides a result on a usual machine: a
/// query that it stops gives [`SAFETY_CAUSE`].
pub const SAFETY_QUERY: Duration = Duration::from_secs(60);

/// The cause of a check that its budget in solver steps stops.
pub const BUDGET_CAUSE: &str = "the budget of the step";

/// The cause of a check that the safety time limit stops (a separate cause, never mixed with the
/// budget).
pub const SAFETY_CAUSE: &str = "the safety time limit";

/// The cause of a query that reaches [`QUERY_STEPS`].
pub const QUERY_LIMIT_CAUSE: &str = "the step limit of one query";

/// The budget of one check: the most solver steps, and the safety time limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub steps: u64,
    pub safety: Option<Instant>,
}

impl Budget {
    /// A budget of `steps` solver steps with no safety time limit.
    pub fn steps(steps: u64) -> Budget {
        Budget { steps, safety: None }
    }

    /// A budget with a safety time limit.
    pub fn with_safety(steps: u64, safety: Option<Instant>) -> Budget {
        Budget { steps, safety }
    }
}

thread_local! {
    /// The solver steps of this thread since its start (each query and each group of steps of
    /// the machine).
    static USED: Cell<u64> = const { Cell::new(0) };
    /// The end of the budget of the check of this thread: the count of [`USED`] where it stops,
    /// and the safety time limit.
    static END: Cell<Option<(u64, Option<Instant>)>> = const { Cell::new(None) };
}

/// The solver steps of this thread since its start.
pub fn used() -> u64 {
    USED.with(Cell::get)
}

/// Counts solver steps of this thread.
pub fn charge(steps: u64) {
    USED.with(|cell| cell.set(cell.get().saturating_add(steps)));
}

/// The rest of the budget of this thread in solver steps, or `None` with no budget.
pub fn rest() -> Option<u64> {
    END.with(Cell::get).map(|(end, _)| end.saturating_sub(used()))
}

/// The cause that stops the check of this thread: the safety time limit first, then the budget.
pub fn stop() -> Option<&'static str> {
    let (end, safety) = END.with(Cell::get)?;
    if safety.is_some_and(|safety| Instant::now() >= safety) {
        return Some(SAFETY_CAUSE);
    }
    (used() >= end).then_some(BUDGET_CAUSE)
}

/// True if a cause is the stop of a budget or of the safety time limit.
pub fn is_stop(cause: &str) -> bool {
    cause == BUDGET_CAUSE || cause == SAFETY_CAUSE
}

/// Runs `f` with a budget. A nested call keeps the smaller budget and the earlier safety time
/// limit of its outer call. `None`: the budget of the outer call (no budget at the top). Returns
/// the result and the solver steps that `f` used.
pub fn with<R>(budget: Option<Budget>, f: impl FnOnce() -> R) -> (R, u64) {
    let outer = END.with(Cell::get);
    let start = used();
    let own = budget.map(|budget| (start.saturating_add(budget.steps), budget.safety));
    let end = match (outer, own) {
        (Some((a, x)), Some((b, y))) => Some((a.min(b), match (x, y) {
            (Some(x), Some(y)) => Some(x.min(y)),
            (x, y) => x.or(y),
        })),
        (a, b) => a.or(b),
    };
    END.with(|cell| cell.set(end));
    let found = f();
    END.with(|cell| cell.set(outer));
    (found, used() - start)
}

/// [R80.2-1] The budget of the congruence tries of one body check in solver steps (the budget of
/// one body check, R77.2-2).
pub const CONGRUENCE_STEPS: u64 = 60_000_000;

/// [R80.2-1] Runs `f` with its own budget of `steps` solver steps, apart from the meter of the
/// thread: the steps of `f` do not count in the budget of the outer check, so `f` never changes
/// another result of that check. The safety time limit of the outer check stays.
pub fn apart<R>(steps: u64, f: impl FnOnce() -> R) -> R {
    let outer = END.with(Cell::get);
    let start = used();
    let safety = outer.and_then(|(_, safety)| safety);
    END.with(|cell| cell.set(Some((start.saturating_add(steps), safety))));
    let found = f();
    END.with(|cell| cell.set(outer));
    USED.with(|cell| cell.set(start));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    // rule+ R77.2-1
    #[test]
    fn the_budget_counts_solver_steps_and_a_nested_budget_keeps_the_smaller_one() {
        let ((), spent) = with(Some(Budget::steps(100)), || {
            assert_eq!(rest(), Some(100));
            charge(60);
            assert_eq!(stop(), None);
            let ((), inner) = with(Some(Budget::steps(1000)), || {
                // The outer budget is smaller: 40 steps are left.
                assert_eq!(rest(), Some(40));
                charge(50);
                assert_eq!(stop(), Some(BUDGET_CAUSE));
            });
            assert_eq!(inner, 50);
            assert_eq!(stop(), Some(BUDGET_CAUSE));
        });
        assert_eq!(spent, 110);
        assert_eq!(stop(), None);
        assert_eq!(rest(), None);
    }

    // rule- R77.2-1
    #[test]
    fn the_safety_time_limit_has_its_own_cause() {
        let past = Instant::now() - Duration::from_secs(1);
        let (cause, _) = with(Some(Budget::with_safety(1_000_000, Some(past))), stop);
        assert_eq!(cause, Some(SAFETY_CAUSE));
        assert!(is_stop(SAFETY_CAUSE) && is_stop(BUDGET_CAUSE) && !is_stop(QUERY_LIMIT_CAUSE));
    }
}
