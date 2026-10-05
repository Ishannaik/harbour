# Contributing to harbour

Bug reports, feature ideas, and pull requests are welcome. The TUI is small
on purpose. Before you open a PR, read the constraints below.

## Setup

```bash
cargo build
cargo test
```

## Constraints

- Keep the dependency tree lean. Justify every new crate.
- Match the omp-grade polish bar: consistent keybindings, no dead UI states.
- Follow `cargo fmt` and `cargo clippy -- -D warnings` before pushing.

## Before you pick an issue

Join the [Discord](https://discord.gg/KKvtRhQvRv) before picking an issue; claim it there or comment on the issue.
