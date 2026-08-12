# harbor — shared design context (THE contract for all docs)

Every doc MUST be consistent with this file. Exact names, statuses, keybinds,
sources, and phases below are normative — do not invent alternatives.

## Identity

- **Name**: harbor (crate `harbor`, binary `harbor`)
- **Tagline**: "curated torrents straight from your terminal" (same as torlink — deliberate)
- **License**: MIT (2026, Ishan Naik)
- **Language**: Rust, edition 2024, tokio async
- **Reference product**: torlink (github.com/baairon/torlink, TypeScript/Ink, MIT) — v1 targets **feature parity with torlink's interactive app**
- **Vibe**: omp-level terminal polish — animated, truecolor, zero flicker

## What harbor does (v1 = torlink parity)

- Opens straight to a search bar. Type query + Enter → results stream in from 10 sources as each answers, tagged with size and seeders.
- Empty query + Enter → browse curated top lists.
- Arrow keys navigate; `d` downloads to the default folder; `shift+d` picks a folder for that download; `o` changes the output folder; `p` pauses/stops seeding; `?` shows keybinds; `Tab` cycles screens (search ⇄ downloads); `←`/`→` switch the downloads tabs; `esc` closes help; `q` quits.
- Downloads run in the background while you keep searching; queue supports unlimited items, concurrency cap.
- Active downloads show progress, speed, time left; finished items drop into "recently downloaded"; everything persists across restarts; interrupted downloads resume.
- Finished torrents keep seeding by default (opt out per-item with `p`); Seeding tab shows upload speed, peers, pause/stop.
- CLI: `harbor [magnet|infohash|.torrent]` launches straight into that download; `--help`, `--version`.

## Sources (10, same as torlink)

| id | label | groups | kind |
| --- | --- | --- | --- |
| gameshub | GamesHub | Games | HTML scrape |
| cinevault | CineVault | Movies | JSON API (cinevault.mx/.am/.rs fallback hosts) |
| vault-movies | VaultIndex | Movies | JSON API (mirror-api.org) |
| reel-movies | ReelIndex | Movies | HTML scrape |
| showport | ShowPort | TV | RSS |
| vault-tv | VaultIndex | TV | JSON API (mirror-api.org) |
| reel-tv | ReelIndex | TV | HTML scrape |
| tsukibase | TsukiBase | Anime | RSS |
| fansubs | FanSubs | Anime | RSS |
| torrent-hub | TorrentHub | Movies | HTML scrape |

Games are the only category that runs code → GamesHub alone (trusted repacker). If a source is down, search continues without it and the sidebar reports it offline.

- `Source` trait: `{ id, label, groups, homepage, reports_health, search(query) -> Vec<TorrentResult> }`
- `TorrentResult`: `{ info_hash, name, size_bytes, seeders, leechers, num_files?, source, magnet, added? }`
- Multi-host fallback per source; resilient fetch (retries, per-source timeout, abort signal); dead source → `offline` in sidebar, never blocks others.
- Magnet builder: `magnet:?xt=urn:btih:<lowercase infohash>&dn=<name>`.
- Search-result cache: per (source, query), 5-minute TTL.

## Engine

- **librqbit** (embedded, Apache-2.0) — magnet/.torrent/infohash input, metadata, DHT + trackers, stats, seeding, session-state resume.
- **Queue statuses**: `queued → downloading → failed`; on done → moves to seeding (`seeding → missing`).
- Concurrency cap via `HARBOR_MAX_DOWNLOADS` env (0 / unset = unlimited); oldest-first `promote()` when a slot frees.
- 500ms stats poll.
- **Bootguard**: crash marker written at boot; if the previous run died mid-restore, restore every item paused (safe mode) — no engines start until the user resumes.
- **Metadata capture**: when torrent metadata arrives, save the `.torrent` bytes → later re-add/re-seed verifies on-disk files locally without re-fetching from the swarm.
- **Stray-download detector**: a seed reporting `speed > 0 && progress < 1` for 2 consecutive polls after a 10s grace period = files missing → flag `missing`.
- Seed-by-default after completion; trackers override supported.

## UI stack (stolen from the omp harness)

- ratatui + crossterm + tokio.
- Differential rendering (ratatui's diff — only changed cells rewritten).
- **30fps base cadence**, coalesced render requests, adaptive backpressure from previous frame cost.
- **DEC 2026 synchronized output** (BSU/ESU) around every frame — zero flicker.
- Loader: 80ms spinner advance (~12.5fps status / ~30fps activity); animated colorizers on status line.
- Eased progress bars (values ease toward target, never jump); speed/ETA tick at 30fps.
- Rounded borders `╭╮╰╯` + tee junctions.
- **Views**: splash (animated logo draw-in + gradient sweep) → search (sidebar: groups + source-health dots, gradient search bar with shimmer while results stream, results with size/seeders colored, staggered source tags) → downloads (active animated bars + speed/peers/ETA, recently downloaded, Seeding tab) → now-playing (phase 6, libmpv).
- Alt-screen lifecycle; hide hardware cursor (TUI draws its own); synchronous exit — never wait on engine sockets, OS reclaims them; restore terminal unconditionally on exit.
- Crash logging to a file; terminal always restored on panic/exit.

## Theme (stolen from omp's schema, ported to Rust)

- omp theme JSON schema ported verbatim: `name`, `colors` (required tokens), `vars` (recursive refs), `symbols` (preset `unicode|nerd|ascii`, per-key overrides, `spinnerFrames`), optional `export`.
- Default dark theme: **titanium** (Tokyo Night palette): accent `#7aa2f7`, success `#9ece6a`, error `#f7768e`, warning `#e0af68`, muted `#565f89`, dim `240`, text `#c0caf5`, selectedBg `#2a2f45`, bg/statusLineBg `#16161e`, border `#4c566a`, syntaxComment `#565f89`, syntaxKeyword `#bb9af7`, syntaxFunction `#7aa2f7`, syntaxString `#9ece6a`, syntaxNumber `#ff9e64`, syntaxType `#2ac3de`, syntaxOperator `#89ddff`, syntaxPunctuation `#9aa5ce`.
- Full token list (from omp): core text/borders (accent, border, borderAccent, borderMuted, success, error, warning, muted, dim, text, thinkingText), background blocks (selectedBg, userMessageBg, customMessageBg, toolPendingBg, toolSuccessBg, toolErrorBg, statusLineBg), message/tool text (userMessageText, customMessageText, customMessageLabel, toolTitle, toolOutput), markdown (mdHeading, mdLink, mdLinkUrl, mdCode, mdCodeBlock, mdCodeBlockBorder, mdQuote, mdQuoteBorder, mdHr, mdListBullet), diff+syntax (toolDiffAdded, toolDiffRemoved, toolDiffContext, syntaxComment, syntaxKeyword, syntaxFunction, syntaxVariable, syntaxString, syntaxNumber, syntaxType, syntaxOperator, syntaxPunctuation), thinking/borders (thinkingOff, thinkingMinimal, thinkingLow, thinkingMedium, thinkingHigh, thinkingXhigh, bashMode, pythonMode), status line (statusLineSep, statusLineModel, statusLinePath, statusLineGitClean, statusLineGitDirty, statusLineContext, statusLineSpend, statusLineStaged, statusLineDirty, statusLineUntracked, statusLineOutput, statusLineCost, statusLineSubagents).
- harbor uses a curated subset of these tokens for its views; the theming doc MUST present the full schema with a clear "used by harbor" annotation per token, and note which tokens are omp-app-specific.
- Custom theme dir: `~/.harbor/themes/<name>.json`; live reload on file change; validation errors loud with fallback to defaults; color-mode detection: `COLORTERM=truecolor` or `WT_SESSION` → truecolor, else 256-color.

## Persistence

- Config dir: `~/.harbor/` (Windows: `%USERPROFILE%\.harbor`).
- Files: `config.toml`, `downloads.json` (ledger), `history.json` (cap 500), `cache/search/<source>/<query>.json`, `cache/torrents/<id>.torrent`, `cache/covers/`.
- librqbit session state handles piece-level resume.

## Concurrency model

- tokio runtime. Engine events → mpsc channel → UI state; UI renders from state at 30fps; input events → actions → engine.
- All network ops async and non-blocking; per-source isolation.

## Error handling

- Per-source failure → `offline` tag, search continues.
- Engine errors → error banner (omp errorBanner style) + item status `failed` with message.
- Config/theme validation errors → loud, fall back to defaults with warning.
- Panic safety: crash log to file; terminal always restored.

## Testing

- Scraper unit tests on fixture HTML/JSON/RSS.
- Theme-schema validation tests; cache TTL tests.
- ratatui buffer-snapshot tests per view.
- Animation determinism (fixed-tick tests).
- Integration: real tiny magnet gated behind `HARBOR_TEST_NET=1`.
- Manual smoke against live sources before release.

## Build order (normative phases for roadmap.md)

1. Skeleton: crate, theme loader, animation loop (30fps + sync output), terminal lifecycle.
2. Splash + search UI with fake data.
3. Real scrapers + cache.
4. librqbit integration: download/progress/seed.
5. Persistence + bootguard + resume.
6. Watch mode: libmpv + librqbit HTTP stream endpoint (tori stack).
7. Deferred spikes: cs.rin.ru, online-fix.me, cover art (sixel/halfblocks), headless daemons.

## Explicit v1 non-goals (deferred)

- cs.rin.ru / online-fix.me sources (scraping feasibility unproven — Cloudflare, forum/catalog structure; spike in phase 7).
- Live streaming watch (phase 6, via libmpv + librqbit Range-served stream; no custom render engine — external player/libmpv is the renderer).
- Cover art / inline images.
- Headless daemon modes (`watch`/`serve`/`files`/`attach` + `--daemon`).
- Built-in updater.

## Code comment convention (user requirement: "make sure the comments")

- Comments explain **why**, not what (what is evident from code).
- Non-obvious invariants, failure modes, and tradeoffs get a comment at the decision site.
- `///` rustdoc on public items; `//` on internals; `TODO`/`FIXME` linked to issues where possible.
- Reference style: torlink's `engine.ts` comments (native-port tone), omp's `tui-core-renderer.md` invariants.

## Doc deliverables (each written by one parallel subagent)

1. `README.md` — root, marketing + quickstart
2. `SPEC.md` — root, complete spec sheet (testable requirements)
3. `docs/architecture.md`
4. `docs/design.md`
5. `docs/roadmap.md`
6. `docs/sources.md`
7. `docs/theming.md`

Repo has no code yet — docs describe the intended implementation. Nothing exists in the repo beyond: LICENSE, .gitignore, docs/ (empty), this context (not committed — scratch).
