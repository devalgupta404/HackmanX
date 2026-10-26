//! The source side: probe and preflight, the used-ranges reader, the pass loop driven by the
//! convergence model, and quiesce (hooks, swapoff, FIFREEZE, the drain check). Reads the source
//! read-only through `bootshift-disk` (invariant M1). Built out in plan.md Phases 5–7.
//!
//! Skeleton: lands on top of `migrate-core` and `migrate-net`.
