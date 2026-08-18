# UPnP / NAT-PMP, random port, listening port and interface binding
Ref: #55

## Goal
Give the listening socket a full, honest set of controls — port, random-port-per-launch, bind
address, UPnP — and show the port harbour is *actually* listening on, instead of only the port
the user asked for.

## The finding that shapes this plan

Read on **2026-08-16** from the current release, `librqbit 9.0.0`
(`crates.io/api/v1/crates/librqbit/9.0.0/download`, extracted to `/tmp/librqbit-9.0.0`), plus
its transitive socket crate.

**All four listener knobs live in one struct** — `ListenerOptions`, `listen.rs:52-60`:

```rust
pub struct ListenerOptions {
    pub mode: ListenerMode,
    pub listen_addr: SocketAddr,          // bind IP *and* port, together
    pub enable_upnp_port_forwarding: bool,
    pub utp_opts: Option<librqbit_utp::SocketOpts>,
    pub announce_port: Option<u16>,
    pub ipv4_only: bool,
    pub max_pending_incoming_handshake_checks: usize,
}
```

This replaces 8.1.1's `SessionOptions.listen_port_range: Option<Range<u16>>` and
`SessionOptions.enable_upnp_port_forwarding`, which is why **this issue depends on the librqbit
9 upgrade described as step 0 of `docs/plans/protocol-toggles.md`** and must not restate it.

**Port 0 already does exactly what "random port" needs.** `ListenerOptions::start`,
`listen.rs:99-113` and `:147-153`:

```rust
let listener = TcpListener::bind_tcp(listen_addr, ..)?;
listen_addr = listener.bind_addr();          // the OS-assigned port, read back
...
let announce_port = if let Some(p) = self.announce_port { Some(p) }
    else if listen_addr.ip().is_loopback() { None }
    else { Some(listen_addr.port()) };        // trackers get the *real* port
```

So harbour does not need to roll a random number: bind `:0`, and librqbit binds, reads back and
announces the effective port. `Session::announce_port() -> Option<u16>` is **public**
(`session.rs:1631-1633`), so the TUI can display it.

**NAT-PMP does not exist in librqbit, at any version.**

```
$ grep -rn -i "natpmp|nat_pmp|pcp" /tmp/librqbit-9.0.0/     # 0 matches
```

Only UPnP-IGD is implemented (`enable_upnp_port_forwarding`, plus the unrelated
`upnp_server_adapter.rs`, which is the *media server*, not port mapping). **No NAT-PMP toggle is
built** — see FR-104.

**Interface-name binding is Unix-only and fails the session on Windows.** `SessionOptions`
has `bind_device_name: Option<String>` (`session.rs:425`), mapped at `session.rs:583-589`:

```rust
let bind_device = match opts.bind_device_name.as_ref() {
    Some(name) => Some(BindDevice::new_from_name(name)
        .with_context(|| format!("error creating bind device {name}"))?),
    None => None,
};
```

and `BindDevice::new_from_name` in `librqbit-dualstack-sockets 0.7.0`
(`src/bind_device.rs:26-29`) is:

```rust
#[cfg(windows)]
pub fn new_from_name(name: &str) -> crate::Result<Self> {
    Err(Error::BindDeviceNotSupported)
}
```

The `?` on that line means **on Windows, setting an interface name makes `Session::new_with_opts`
fail outright.** In harbour that surfaces via `src/app/mod.rs:370-383` as
`"downloads are unavailable: …"` and a `FakeEngine` — the user typed an interface name into
settings and lost all downloading, on the platform NFR-08 names as the primary target.
`reqwest`'s equivalent `.interface()` is likewise `#[cfg(not(windows))]` in librqbit's own client
builder (`session.rs:713-717`).

**Therefore harbour ships bind-by-IP-address, not bind-by-interface-name.** `listen_addr`'s IP
half is portable, works on all three platforms with no conditional compilation (NFR-08), and
covers the actual use case — "only accept peers on my VPN adapter" — because a VPN adapter has
an address. Interface-name binding is a documented non-goal (FR-104) until upstream supports
Windows.

## SPEC / FR reference

**SPEC.md says nothing about ports, binding or port forwarding.** `grep -n -i "port" SPEC.md`
matches only "portability" (NFR-08/09) and "supported". FR-51's config list does not mention
the `listen_port` / `enable_upnp` rows that already ship. **SPEC first**, per AGENTS rule 2.

FR numbers **FR-101 … FR-104** (FR-69…FR-100 claimed by existing plans plus this batch's
`protocol-toggles.md` and `encryption-mode.md`). Add to §4.5 "Connection & protocol".

- **FR-101 (listening port).** harbour listens for incoming peers on a configured TCP port.
  Empty means the operating system assigns one. The port is applied at engine start; the
  settings row states that changing it takes effect at the next launch.
- **FR-102 (random port).** When "use a random port each launch" is on, harbour requests an
  OS-assigned port at every start and ignores the configured port. The **effective** port —
  the one actually bound and announced to trackers — is displayed in settings, so the user can
  always see what to forward. A random port and UPnP together are the intended combination; a
  random port without port forwarding means no incoming connections.
- **FR-103 (bind address).** harbour can bind its listening socket to one local IP address.
  Empty means all addresses. An address that cannot be bound is a **loud startup failure with
  the address named**, never a silent fall back to binding everything — falling back would leak
  traffic onto the interface the user was trying to exclude, which is the whole reason to set
  it.
- **FR-104 (port forwarding).** harbour exposes UPnP-IGD port forwarding as an on/off setting,
  default on. **NAT-PMP and PCP are not supported**: the engine implements neither
  (librqbit 9.0.0, verified 2026-08-16), and harbour does not implement port mapping itself.
  Binding to a network *interface by name* is likewise unsupported: the underlying socket crate
  returns `BindDeviceNotSupported` on Windows, and a control that breaks the engine on the
  primary platform is not shipped. Bind by IP address (FR-103) instead. Both are re-evaluated
  on each librqbit upgrade.

## Workstream

- **Step 0 (librqbit 9 upgrade)** — **Engine (Sarthak)**; specified once in
  `docs/plans/protocol-toggles.md`. Blocks this issue.
- **Steps 1–3 (SPEC, config, engine mapping)** — **Engine & Foundation (Sarthak)**.
- **Steps 4–5 (settings rows, effective-port display)** — **Terminal UI (Ishan)**.

**Shared types:** one addition to the frozen contract in `src/core/types.rs` — an
`Engine::listen_port(&self) -> Option<u16>` accessor for step 5. It follows the
`set_speed_limits` precedent exactly (`src/core/types.rs:694`): an **additive default** returning
`None`, so the trait stays frozen and every implementor keeps compiling. Sarthak owns it; the UI
builds against it after it lands.

**Row-table prerequisite** (stated identically in all five plans of this batch): the settings
row-table refactor from step 1 of `docs/plans/speed-limits.md` (#43) / `categorized-settings.md`
(#63) lands first, so rows are identified by value, not index. The agreed final Connection-block
order is listed in `docs/plans/protocol-toggles.md` and this issue contributes rows 1–5 of it.

## Approach

**Step 1 — SPEC FR-101…FR-104 (docs only, ~45 lines).**

**Step 2 — config fields (engine, ~50 lines).**

`Config` (`src/persist.rs:41-89`) keeps `listen_port: Option<u16>` and `enable_upnp: bool`
unchanged, and gains two:

```rust
/// Ask the OS for a port at every launch, ignoring `listen_port`. Boot-time.
pub random_listen_port: bool,          // default: false
/// Local IP to bind the listening socket to; None = all addresses. Boot-time.
pub bind_address: Option<String>,      // default: None
```

`bind_address` is a `String`, not an `IpAddr`: `Config` is `Serialize + Deserialize` into TOML
and a malformed address must produce a **loud, keep-editing** warning at the settings row (the
`parse_opt_number` pattern at `src/app/settings.rs:78-87`), not a whole-config parse failure
that would quarantine the user's file over one typo.

**Step 3 — the engine mapping (engine, ~70 lines).**

`EngineLaunchOptions` (`src/engine/rqbit.rs:581`) gains `random_listen_port: bool` and
`bind_address: Option<String>`, and `RqbitEngine::new` computes one `SocketAddr`. All librqbit
types stay inside this module, per its `//!` contract:

```rust
// Port 0 asks the OS for a free port; librqbit reads the bound port back
// (listen.rs:110) and announces the real one, so "random" needs no RNG here.
let port = if opts.random_listen_port { 0 } else { opts.listen_port.unwrap_or(0) };
let ip: IpAddr = match opts.bind_address.as_deref() {
    // An unparseable bind address is a hard error: silently binding all
    // interfaces would leak traffic onto the one the user excluded.
    Some(s) => s.parse().map_err(|_| EngineError::InvalidInput(..))?,
    None => IpAddr::V6(Ipv6Addr::UNSPECIFIED),   // librqbit's dual-stack default
};
listen: Some(ListenerOptions { listen_addr: SocketAddr::new(ip, port),
                               enable_upnp_port_forwarding: opts.enable_upnp,
                               ..Default::default() }),
```

Two invariants get a why-comment at this decision site:

1. `listen: None` (the 9.0.0 `Default`, `session.rs:493`) means **no listener and no UPnP at
   all** — this `Some(..)` is mandatory, not stylistic.
2. Binding a loopback address makes librqbit announce **no** port (`listen.rs:149-151`), so a
   user who binds `127.0.0.1` gets outbound-only operation. Step 5's display shows `not
   announced` rather than a number, which is the honest rendering of that state.

**Step 4 — settings rows (UI, ~90 lines).**

Rows 1–5 of the agreed Connection block:

```
Listening Port (empty = auto)      text     exists — label unchanged
Use a Random Port Each Launch      toggle   new
Bind Address (empty = all)         text     new
UPnP Port Forwarding               toggle   exists — label unchanged
Effective Listening Port           info     new (step 5)
```

When `random_listen_port` is on, the `Listening Port` row's **value** renders `random each
launch` instead of the stored number — the stored number is kept in config (turning random off
restores it) but the row must not display a port that is not in use. That is the same honesty
rule as FR-102's effective-port display, applied to the row above it.

`Bind Address` validation happens at commit, in `settings_edit_text`
(`src/app/settings.rs:93-240`), reusing the established shape: `"…".parse::<IpAddr>()`, and on
failure `app.warn(format!("'{value}' is not an IP address — leave empty for all"))` with the
edit left open. Never a silent reset.

**Step 5 — show the effective port (UI + engine, ~60 lines).**

`Engine::listen_port()` returns `self.session.announce_port()` in `RqbitEngine`
(`session.rs:1631`), `None` in `FakeEngine`. A read-only `Info` row renders:

- a number (`51413`) when bound and announced
- `not announced (bound to loopback)` when the bind address is loopback
- `engine unavailable` when the engine failed to start

**This row is the feature.** Everything above it is settings plumbing that a user cannot verify;
this is the line that tells them which port to forward and proves the random port worked. It
reuses the `RowKind::Info` variant introduced by `docs/plans/encryption-mode.md` (#54) — whichever
lands first adds the variant, the second one just adds a row.

## Files to create / modify

- `SPEC.md` — FR-101…FR-104 in §4.5.
- `src/persist.rs` — `random_listen_port` (default false), `bind_address` (default None);
  round-trip + partial-config tests.
- `src/core/types.rs` — `Engine::listen_port()` with an additive `None` default (Sarthak).
- `src/engine/rqbit.rs` — `EngineLaunchOptions` fields; the `SocketAddr` computation and its two
  why-comments; `listen_port()` over `Session::announce_port()`.
- `src/engine/fake.rs` — `listen_port()` returns `None`, so the UI's "unavailable" path is
  exercised by the existing fake-engine tests.
- `src/ui/settings.rs` — two new rows + the effective-port `Info` row; the `random each launch`
  value rendering for the port row.
- `src/app/settings.rs` — the `RandomPort` toggle arm; `BindAddress` text commit with `IpAddr`
  validation.
- `src/app/mod.rs` — pass the effective port into the settings draw call (it already threads
  `&app.config` and `&app.settings`; this is one more read from `app.queue.engine()`).
- `src/ui/tests.rs` — snapshots for all three states of the effective-port row.

## Key APIs / libraries

Verified 2026-08-16 by reading extracted crate sources:

- `ListenerOptions` — `librqbit-9.0.0/src/listen.rs:52-60`; `Default` at `:62-75`
  (`listen_addr: (Ipv6Addr::UNSPECIFIED, 0)`, `enable_upnp_port_forwarding: false`).
- Port readback and announce logic — `listen.rs:99-113`, `:147-153`.
- `Session::announce_port()` — `session.rs:1631-1633`, public.
- `bind_device_name` mapping — `session.rs:583-589`; `BindDevice::new_from_name` Windows arm —
  `librqbit-dualstack-sockets-0.7.0/src/bind_device.rs:26-29`
  (`Err(Error::BindDeviceNotSupported)`).
- No NAT-PMP/PCP anywhere in the crate (grep, 0 matches).
- librqbit 9.0.0 is current stable — `crates.io/api/v1/crates/librqbit`
  (`max_stable_version 9.0.0`, `updated_at 2026-08-15`).

**New crates: none.** `IpAddr`/`SocketAddr` parsing is `std`. A NAT-PMP crate is explicitly
rejected below.

## Risks / edge cases

- **Rejected: a `natpmp` crate in harbour.** Port mapping has to be renewed on a timer, undone
  on shutdown, and kept consistent with the port the engine actually bound — that lifecycle
  belongs to whoever owns the socket, which is librqbit. A harbour-side mapper would be a second
  owner of the same resource, would drift on every port change, and adds a dependency for a
  half-feature. AGENTS rule 8. File an upstream issue instead and link it from FR-104.
- **Rejected: a "network interface" text row.** On Windows it is a guaranteed engine-start
  failure (source above), and NFR-08 forbids a platform-conditional feature set. Bind-by-IP is
  the portable equivalent and is what step 4 ships.
- **A wrong bind address must fail loudly.** If `192.168.1.50` is stale because DHCP moved,
  `Session::new_with_opts` errors and harbour shows `"downloads are unavailable: …"`. That is
  correct and intended — the alternative (bind everything) silently defeats the setting. The
  banner text must include the address so the fix is obvious. This is the single most likely
  support question from this issue; write the message carefully.
- **Loopback bind = no incoming peers, by design.** librqbit refuses to announce a loopback port
  (`listen.rs:149-151`). The effective-port row must say `not announced`, not `0` or `—`.
- **Random port + UPnP is the combination that works; random port alone is not "safer".** The
  FR-102 sentence exists so the settings `?` detail text (#63) can say it. A user who turns on
  random ports expecting privacy and loses all incoming peers has been misled by omission.
- **Do not confuse UPnP-IGD with `upnp_server_adapter.rs`.** The latter is librqbit's DLNA/media
  server and is unrelated. A reviewer skimming for "upnp" will find both.
- **Port conflicts.** Explicit port already in use → `bind_tcp` fails → the engine does not
  start. Loud, correct, and the reason "empty = auto" is the default. Worth naming in FR-101's
  detail text so the fix (clear the port) is discoverable.
- **`ipv4_only` is deliberately not exposed.** Default dual-stack is right for nearly everyone
  and it is a fourth axis on an already-busy panel. Out of scope; named so it is not re-proposed.

## Test strategy

- **Unit, `src/engine/rqbit.rs`** — a pure helper
  `fn listen_socket_addr(random: bool, port: Option<u16>, bind: Option<&str>)
   -> Result<SocketAddr, EngineError>`, so every case is testable without a `Session`:
  random ⇒ port 0 regardless of the configured port; no port + not random ⇒ 0;
  `Some(51413)` ⇒ 51413; `"192.168.1.5"` ⇒ that IPv4 with the right port; `"::1"` ⇒ IPv6;
  `"not-an-ip"` ⇒ `Err(InvalidInput)`, **never** a default address.
- **Unit, `src/persist.rs`** — both new fields round-trip; a config file written before this
  change loads with `random_listen_port = false` and `bind_address = None`.
- **Unit, `src/app/settings.rs`** — committing `"999.1.1.1"` into `Bind Address` leaves
  `config.bind_address` unchanged, keeps `editing == true`, and raises a warning (the
  established never-guess contract).
- **Buffer snapshot, `src/ui/tests.rs`** — with `random_listen_port = true`, the port row's value
  reads `random each launch` and **not** a number; the effective-port row renders a number when
  the engine reports one, `not announced (bound to loopback)` for a loopback bind, and
  `engine unavailable` for `FakeEngine`.
- **Integration, `HARBOUR_TEST_NET=1`, `tests/engine_net.rs`** — start an engine with
  `random_listen_port = true`, assert `listen_port()` is `Some(p)` with `p != 0`; start a second
  with an explicit free port and assert `listen_port() == Some(that port)`. Bind-address cases
  stay out of the net test (they are environment-dependent); the pure helper covers them.

## Verification

1. `cargo run` → `shift+S` → Connection block shows Listening Port, Use a Random Port Each
   Launch, Bind Address, UPnP Port Forwarding, Effective Listening Port, in that order.
2. Set an explicit port (e.g. `51413`), relaunch, and the **Effective Listening Port row shows
   `51413`** — and `netstat -ano | findstr 51413` (Windows) shows harbour listening. That match
   between the row and the OS is the proof the whole plan exists for.
3. Turn on random port, relaunch twice: the effective port differs between runs, and the
   Listening Port row reads `random each launch` rather than a stale number. Turn random off:
   the previously configured port comes back, unlost.
4. Set Bind Address to the machine's LAN IP, relaunch, download something — it works, and
   traffic is on that adapter. Set it to `127.0.0.1`, relaunch: the effective-port row reads
   `not announced (bound to loopback)`.
5. Type `banana` into Bind Address and press Enter: a warning banner names the value, the edit
   stays open, and `~/.harbour/config.toml` is unchanged.
6. Set Bind Address to an address the machine does not have, relaunch: a loud
   `"downloads are unavailable: …"` banner **naming the address**, not a silent bind-everything.
7. `grep -rn -i "natpmp\|bind_device_name" src/` returns nothing — neither non-goal was shipped,
   and SPEC FR-104 says why.
