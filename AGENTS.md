# AGENTS.md — harbour collaboration contract

Everyone working in this repo — human or AI agent — follows this. It is the
handshake between the three workstreams.

## The three workstreams (skill-aware)

| Track | Owner | Scope | Roadmap phases |
| --- | --- | --- | --- |
| **Terminal UI** | Ishan | theme schema, animation loop, splash / search / downloads views, now-playing panel | 1 (UI parts), 2, 6 (UI) |
| **Engine & Foundation** | Sarthak (expert) | crate skeleton, shared-types freeze, librqbit integration, queue, persistence, bootguard, resume | 1 (types), 4, 5 |
| **Sources & Cache** | Dhruv | 10 scrapers, magnet builder, resilient fetch, search cache, offline health | 3, 7 (spikes) |

- **Sarthak (expert)** owns the load-bearing contracts and reviews all PRs.
- **Dhruv (newbie)** takes the sources on-ramp: RSS first (ShowPort, TsukiBase) → JSON (CineVault, VaultIndex) → HTML (ReelIndex, GamesHub, TorrentHub). Each scraper is a complete, testable deliverable on its own.
- **Ishan** owns the UI. Shared types belong to Sarthak (phase 1); the UI builds against the frozen types on fake data until integration.

## Commands

```bash
cargo build
cargo run
cargo test
cargo fmt
cargo clippy -- -D warnings
```

Quality gates (FR-62..FR-66): CI enforces fmt/clippy/tests/audits on every PR; `just check` runs the identical pipeline locally. Max line length 100 (rustfmt.toml + CI check), no exemptions. Size/complexity norms (FR-67): nesting >4, cognitive complexity >15, and functions >120 LOC fail CI (clippy.toml backstop); files >700 LOC earn a split unless an FR-67 exception applies.

## Rules

1. **Never push to `main`.** Branch per workstream: `ui/*`, `engine/*`, `sources/*`. Integrate via PR with ≥1 review (Sarthak reviews engine + sources; anyone reviews UI).
2. **SPEC.md is the referee.** Disagreement → SPEC wins. Behavior missing from SPEC → add it to SPEC first, then implement.
3. **Read before coding:** `SPEC.md`, `docs/design.md`, `docs/architecture.md`, and your track's doc (`docs/sources.md` or `docs/theming.md`).
4. **Shared-types freeze** (`TorrentResult`, `Source` trait, `QueueStatus`, engine event enum) is Sarthak's, lands in phase 1. Everything compiles against it.
5. **Small PRs** (<400 lines, one concern), description references SPEC/FR numbers.
6. **Comments explain why, not what.** Invariants and tradeoffs get a comment at the decision site. `///` rustdoc on public items.
7. **`cargo fmt` + `cargo clippy -- -D warnings` clean before every commit/PR.** Scraper changes ship with fixture tests.
8. **Lean dependency tree.** Justify every new crate in the PR description.
9. **Never commit** `~/.harbour/` state, `.env`, or personal download data (see `.gitignore`).

## Verification per track

- **UI:** `cargo run` → splash renders, views navigable on fake data; ratatui buffer-snapshot tests.
- **Engine:** integration test gated behind `HARBOUR_TEST_NET=1` with a real tiny magnet; queue/persist unit tests.
- **Sources:** fixture tests per scraper (`docs/sources.md` §parsing); manual smoke: `harbour` search shows results from live sources.

## Shared vocabulary (normative)

- Queue statuses: `queued`, `downloading`, `paused`, `failed`, `seeding`, `missing`.
  One `paused` covers a paused download and a paused seed; `QueueItem.finished`
  tells them apart.
- Env vars: `HARBOUR_MAX_DOWNLOADS`, `HARBOUR_TEST_NET`, `HARBOUR_STATE_DIR` (relocates all state for testing).
- Config dir: `~/.harbour/` (Windows: `%USERPROFILE%\.harbour`).
- Crate `harbour`, binary `harbour`. Repo: github.com/Ishannaik/harbour.
