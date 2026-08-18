# BitTorrent encryption mode (disabled / preferred / enforced)
Ref: #54

## Goal
Establish, in SPEC and in the interface, that harbour does **not** encrypt peer traffic —
because the engine cannot — instead of shipping a three-way mode selector that changes nothing.

## The finding that shapes this plan

Read on **2026-08-16**, from source, not documentation.

**librqbit has no protocol-encryption implementation at any version.** Verified by grepping the
entire extracted crate of the current release (downloaded from
`crates.io/api/v1/crates/librqbit/9.0.0/download`) and its peer-protocol dependency:

```
$ grep -rn -i "encrypt|rc4|diffie|\bmse\b|obfuscat" /tmp/librqbit-9.0.0/            # 0 matches
$ grep -rn -i "encrypt|rc4|diffie|\bmse\b|obfuscat" /tmp/librqbit-peer-protocol-9.0.0/src/   # 0 matches
```

MSE/PE (BEP-unnumbered, the uTorrent/Azureus "Message Stream Encryption" scheme) lives entirely
inside the peer handshake: an RC4 keystream negotiated by Diffie–Hellman before the BitTorrent
handshake bytes are exchanged. In librqbit that code would be in
`librqbit-peer-protocol-9.0.0/src/lib.rs` (the `Handshake` type) and in
`librqbit-9.0.0/src/peer_connection.rs`. Both contain plaintext handshakes only. The crate has
**three source files** in peer-protocol (`lib.rs`, `double_buf.rs`, `extended/`) — there is no
room for a missing module.

`SessionOptions` (`/tmp/librqbit-9.0.0/src/session.rs:418-482`) has no encryption field.
`ConnectionOptions` (`stream_connect.rs:35-42`) has `proxy_url`, `enable_tcp`, `peer_opts` and
nothing else. `AddTorrentOptions` (`session.rs:245-300`) has none either.

**Therefore:**

1. **There is no engine knob to bind a mode selector to.** Not "not exposed" — not implemented.
2. A `disabled / preferred / enforced` row in harbour's settings would persist a value that no
   code path reads. Under Ishan's standing rule (*no silent fallbacks, stubs, or band-aids;
   fail loudly*) and AGENTS rule 2 (*SPEC is the referee*), **that row is not built**.
3. "Enforced" is the mode that matters — it is what users on throttling ISPs and on
   encryption-required private trackers actually want — and it is the one that would silently
   fail hardest: harbour would report "enforced" while every connection went out in plaintext.
   That is worse than the honest absence.

**This plan therefore ships documentation, one read-only disclosure row, and an upstream issue.
It does not ship a setting.** A plan that shipped a three-way selector here would be a plan to
ship a lie.

## What the user actually wants, and what does answer it

Two distinct motivations hide behind "encryption mode", and only one of them is unaddressable:

| Motivation | Answered by |
| --- | --- |
| ISP throttles BitTorrent by DPI | **Nothing harbour can do today.** MSE is the only mechanism. |
| Do not expose my IP to peers / trackers | **Issue #57 (proxy)** — real, shippable, partially |

The proxy plan (`docs/plans/proxy.md`) is where the privacy half of this issue gets a real
answer, and FR-100 below points at it explicitly so a user reading SPEC is not left at a dead
end. Note carefully: a SOCKS5 proxy does **not** defeat DPI throttling — the peer traffic is
still plaintext BitTorrent inside the tunnel, and the proxy operator sees it. FR-100 must not
imply otherwise.

## SPEC / FR reference

**Nothing in SPEC.md covers encryption.** `grep -n -i "encrypt" SPEC.md` returns nothing.
NFR-10 ("no telemetry, no network calls beyond source fetching, tracker/DHT traffic for the
user's own torrents") is the closest existing statement of network behaviour and is where a
reader would reasonably expect to learn that peer traffic is unencrypted — it does not say so.

FR number **FR-100** (FR-69…FR-99 claimed by existing plans + `protocol-toggles.md`; verified
with `grep -oh "FR-[0-9]\+" docs/plans/*.md`). Add to §4.5 "Connection & protocol", immediately
after FR-98 (the PeX non-goal) — the two are the same kind of statement and belong together.

- **FR-100 (peer traffic is not encrypted).** harbour exchanges BitTorrent peer traffic in
  plaintext. Message Stream Encryption (MSE/PE) is not implemented by the engine
  (librqbit 9.0.0, verified 2026-08-16), so harbour exposes **no** encryption mode setting:
  a control offering `disabled / preferred / enforced` would report a protection that does not
  exist. The settings overlay states the current behaviour as a read-only line. Peers,
  trackers and any network operator on the path can observe which torrents are being
  transferred. Users needing their IP hidden from peers should use the proxy settings
  (FR-108…FR-112); a proxy hides the address, **not** the protocol, and does not defeat
  DPI-based throttling. This FR is re-evaluated on every librqbit major upgrade.

An `NFR-10` amendment is also in scope: one clause noting peer traffic is plaintext, so the
non-functional network statement is not misleading by omission.

## Workstream

- **Step 1 (SPEC)** — docs. **Engine & Foundation (Sarthak)** reviews, since it is an engine
  capability statement.
- **Step 2 (disclosure row)** — **Terminal UI (Ishan)**.
- **Step 3 (upstream issue + re-check trigger)** — **Engine & Foundation (Sarthak)**.

**Shared types: none change.** No `Config` field, no `EngineLaunchOptions` field, no
`core/types.rs` change. That is the point.

**Row-table prerequisite** (stated identically in all five plans of this batch): the settings
row-table refactor from step 1 of `docs/plans/speed-limits.md` (#43) / step 1 of
`docs/plans/categorized-settings.md` (#63) lands first, so rows are identified by value rather
than by integer index. This plan adds one row kind to that table and no index changes anywhere.

## Approach

**Step 1 — SPEC FR-100 + the NFR-10 clause (docs only, ~20 lines).** Merges alone. This is the
deliverable that closes the issue; everything after it is supporting work.

**Step 2 — one read-only disclosure row (UI, ~60 lines).**

The settings table gains a fourth row kind alongside `Text` / `Theme` / `Toggle` / `Source`:

```rust
/// A row that states a fact and cannot be activated — the engine has no
/// knob behind it. Enter is a no-op; the row is still selectable so the
/// `?` detail popup (#63) can explain why the setting is absent.
Info,
```

One entry in the Connection block, rendered in the muted colour rather than the toggle glyph
colours so it never reads as an off switch:

```
Peer Encryption                    not supported by the engine
```

`settings_activate` (`src/app/settings.rs:16-30`) gains an `Info => {}` arm — explicitly a no-op
with a why-comment, not a fallthrough, so a future row kind cannot inherit silence by accident.

**Why a row at all, rather than nothing:** a user who came from qBittorrent will look for this
setting, not find it, and file the issue again. The row is the answer, in the place they look.
It is a *statement*, not a control — the distinction FR-100 draws and the reason it is
`RowKind::Info` and not a disabled toggle. A greyed-out toggle would still look like a switch
someone could turn on.

If review decides even the disclosure row is scope creep, step 1 alone closes the issue and
step 2 is dropped without affecting anything else. It is deliberately separable.

**Step 3 — upstream issue and the re-check trigger (no harbour code).**

- File an issue on `ikatson/rqbit`: implement MSE/PE, minimally as an outgoing "preferred"
  handshake, ideally with an incoming-connection policy. Link it from FR-100 so the FR has a
  live status rather than a permanent "no".
- Add MSE to the librqbit-upgrade checklist in `src/engine/rqbit.rs`'s module docs: the file
  already carries "three mappings worth reading before changing anything"; this becomes a fourth
  note — *if a future librqbit exposes an encryption option, FR-100 is stale and #54 reopens*.
  A note at the decision site is the only mechanism that survives a year of drift.

## Files to create / modify

- `SPEC.md` — FR-100 in §4.5; one clause added to NFR-10.
- `src/ui/settings.rs` — `RowKind::Info`; one table entry; `row_value` returns the fixed
  `"not supported by the engine"` string for it; muted styling (never the `[● ON]` / `[○ OFF]`
  glyph path in `setting_line`, `src/ui/settings.rs:320-326`).
- `src/app/settings.rs` — `RowKind::Info => {}` in `settings_activate`, with the why-comment.
- `src/engine/rqbit.rs` — module-doc note: no encryption at librqbit 9.0.0; re-check on upgrade.
- `src/ui/tests.rs` — snapshot: the row renders, and renders in the muted style.
- `docs/plans/encryption-mode.md` — this file; the living record of the upstream issue.

**No change to:** `src/persist.rs`, `src/core/types.rs`, `Cargo.toml`.

## Key APIs / libraries

- librqbit **9.0.0** — current stable (`crates.io/api/v1/crates/librqbit`:
  `max_stable_version 9.0.0`, `updated_at 2026-08-15`; checked 2026-08-16). Source read from the
  crates.io tarball; **zero** encryption-related identifiers in the crate or in
  `librqbit-peer-protocol 9.0.0`.
- For context on what would have to be implemented upstream:
  [BitTorrent protocol encryption (Wikipedia)](https://en.wikipedia.org/wiki/BitTorrent_protocol_encryption)
  — RC4 keystream over a Diffie–Hellman exchange, negotiated ahead of the BT handshake.
  [qBittorrent #18752](https://github.com/qbittorrent/qBittorrent/issues/18752) is the reference
  three-mode UI this issue is modelled on; both checked 2026-08-16.
- rqbit's own issue tracker ([github.com/ikatson/rqbit/issues](https://github.com/ikatson/rqbit/issues))
  shows no open MSE issue found via search on 2026-08-16 — which is why step 3 files one rather
  than links one.

**New crates: none.** Explicitly rejected below.

## Risks / edge cases

- **Rejected: ship the three-mode row and store it for later.** The single most likely proposal,
  and the worst one. A user who sets "enforced" for a private tracker that requires encryption
  would believe they complied, connect in plaintext, and get banned. Dead config that *looks*
  like protection is strictly worse than no config.
- **Rejected: implement MSE inside harbour.** The handshake happens inside librqbit's
  `peer_connection.rs`, before any byte harbour can see. There is no seam. Doing it means
  forking librqbit — a fork of a several-hundred-crate engine tree, maintained by one person,
  to add a feature upstream would accept as a PR. Wrong repo.
- **Rejected: adding an `rc4`/`crypto` crate to harbour.** Follows from the above: nothing in
  harbour's process ever touches peer socket bytes, so a crypto dependency would have no call
  site. AGENTS rule 8.
- **Rejected: a "warn the user their traffic is unencrypted" startup banner.** Banner space is
  for things the user must act on; a permanent unactionable warning trains people to dismiss
  banners, which is how the FR-45 "your files are gone" banner loses its force.
- **The FR-100 wording must not oversell the proxy.** A proxy hides the IP from peers; it does
  not encrypt the protocol and does not defeat DPI. If the FR blurs that, #57 inherits a promise
  it cannot keep. Reviewer: read that sentence specifically.
- **Staleness is the real long-term risk.** FR-100 is true on 2026-08-16 and could be false the
  week librqbit adds MSE. The engine module-doc note (step 3) is what makes the next upgrade
  re-check it; without that note this document quietly becomes wrong.

## Test strategy

- **Unit, `src/ui/settings.rs`** — `RowKind::Info` rows are never `Text` and never `Toggle`;
  `text_field()` and `toggle_field()` both return `None` for the encryption row (the table
  totality test from the row-table refactor covers this once the variant exists).
- **Unit, `src/app/settings.rs`** — activating the encryption row leaves `Config` **bit-for-bit
  unchanged** (`assert_eq!(before, after)` on the whole struct, which `Config: PartialEq`
  already allows). This is the test that would catch someone later wiring a field to it.
- **Buffer snapshot, `src/ui/tests.rs`** — the overlay renders
  `Peer Encryption` + `not supported by the engine`, and the buffer contains **neither**
  `[● ON]` nor `[○ OFF]` on that row's line (asserted on the line, not the whole buffer, since
  other rows legitimately carry glyphs).
- **No engine test.** There is nothing to integration-test: the assertion is an absence, and
  the grep in "Key APIs" is the evidence. A test that asserted "no encryption happened" would
  be untestable theatre.

## Verification

1. `SPEC.md` contains FR-100, it names librqbit 9.0.0 and the date the claim was verified, and
   it points at FR-108…FR-112 for the IP-privacy case while explicitly *not* claiming the proxy
   encrypts anything.
2. `cargo run` → `shift+S` → the Connection block shows `Peer Encryption   not supported by the
   engine`. Press Enter on it: **nothing happens** — no edit buffer opens, no glyph flips, no
   banner.
3. `diff` `~/.harbour/config.toml` before and after pressing Enter on that row: identical. No
   key was written.
4. `grep -rn -i "encrypt" src/` returns only the disclosure row's label/value, the
   `settings_activate` why-comment, and the engine module-doc note — no config field, no
   `EngineLaunchOptions` field, no `SessionOptions` mapping.
5. An issue exists on `ikatson/rqbit` requesting MSE/PE, and its number is linked from FR-100
   and from this plan.
