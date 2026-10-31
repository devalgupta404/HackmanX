<!--
Thanks for contributing to Kitten. Please read CONTRIBUTING.md first.
Keep PRs small and focused. Do not mix refactors with behaviour changes.
-->

## What this changes, and why



## Safety invariants touched

<!-- Which of the safety invariants does this affect, and how does it keep them? "None" is a valid answer for docs/tooling. -->



## How it is tested

<!-- Which tests prove this? New tests for new behaviour; a failing-then-passing test for a bug fix. -->



## Checklist

- [ ] `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` are clean
- [ ] `cargo test --workspace` passes
- [ ] If this touches `disk`, `engine`, `fs`, or `boot`: `cargo xtask boot-proof` passes
- [ ] No `unwrap` / `expect` / `panic!` added to non-test code in `core`, `disk`, or `engine`
- [ ] The on-disk journal format is unchanged, or its version was bumped with an old-version decode test
- [ ] No attribution trailers (no `Co-Authored-By:` or tool-generated lines)
