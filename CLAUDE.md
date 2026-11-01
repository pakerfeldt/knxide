# knxide

Rust library for talking to a KNX bus over KNXnet/IP. The full plan, protocol primer, milestones
and rules are in [docs/knx-library-brief.md](docs/knx-library-brief.md). Read it before working on
protocol code.

## Status

- Done: M0 (workspace + CI), M1 (addresses, cEMI `L_Data`, TPCI/APCI), M2 (DPT codecs), M3 (KNXnet/IP frames, fuzzing), M4 (sans-IO tunnel + discovery), M5 (fake gateway, loopback tests), M6 (tokio adapter), M7 code (`knxide` CLI; real-hardware test still pending), M8 (routing; multicast test is `--ignored`, see tests/routing.rs), M9 (tunnelling over TCP), M10 (ETS CSV/XML import, `knxide-project`; `.knxproj` not yet). M11 (KNX Secure) deferred: no Secure hardware or keyring to test against; plan kept in [docs/m11-secure-plan.md](docs/m11-secure-plan.md).
- Test hardware: a KNX IP interface (tunnelling, no Secure).

## Crates

| Crate | Role |
|---|---|
| `knxide-core` | addresses, cEMI, TPCI/APCI, DPT codecs. `no_std`, no I/O |
| `knxide-ip` | KNXnet/IP frames + sans-IO state machines. No I/O, no async |
| `knxide-tokio` | async adapter, owns sockets and timers |
| `knxide-cli` | `knxide` binary for validating against real hardware |
| `knxide-project` | ETS group address export import (CSV, XML) |
| `knxide-testkit` | fake gateway and fixtures, `publish = false`; `examples/demo_gateway.rs` plays fake traffic |
| `apps/knxide-monitor` | web console (axum + WebSocket, page in `static/`), `publish = false` |
| `apps/knxide-dashboard` | customisable dashboard (tiles bound to group addresses, `dashboard.json`), `publish = false` |
| `apps/knxide-proxy` | tunnelling proxy: one upstream tunnel shared by many clients; server side is `knxide_ip::server` |

## Commands

Same checks CI runs:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --all-features
cargo build -p knxide-core --target thumbv7em-none-eabihf   # no_std guard
cargo +1.96 check --workspace --all-targets                  # MSRV (rust-version in Cargo.toml)
cargo deny check                                             # licences and advisories
```

Fuzzing (nightly; `fuzz/` is outside the workspace). Targets: `cemi`, `knxip_frame`, `dpt`, `text`, `project`.

```sh
cd fuzz && cargo +nightly fuzz run --target x86_64-unknown-linux-gnu <target> -- -max_total_time=30
```

## Rules (summary of brief §9)

- License is MIT OR Apache-2.0. Read GPL projects (Calimero, knx-rs, knxd) for behaviour only; never
  copy or closely translate their code.
- Implement from the KNX spec. Doc-comment every protocol constant and struct with its spec
  reference (part and section).
- Decoders never panic. `knxide-core` and `knxide-ip` enable clippy's no-panic lints
  (`unwrap_used`, `indexing_slicing`, …); use fallible access instead.
- No I/O, async or global state in `knxide-core` or `knxide-ip`.
- Allowed dependencies: `thiserror`, `tracing`, `tokio` (adapter only), `proptest` (dev). Ask before
  adding anything else, and add its licence to `deny.toml` only if needed.
- Check in with the owner after M4 (API review) and M7 (real-hardware test).
