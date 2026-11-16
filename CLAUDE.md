# knxide

Rust library for talking to a KNX bus over KNXnet/IP. The full plan, protocol primer, milestones
and rules are in [docs/knx-library-brief.md](docs/knx-library-brief.md). Read it before working on
protocol code.

## Status

- Done: M0 (workspace + CI), M1 (addresses, cEMI `L_Data`, TPCI/APCI), M2 (DPT codecs), M3 (KNXnet/IP frames, fuzzing), M4 (sans-IO tunnel + discovery), M5 (fake gateway, loopback tests), M6 (tokio adapter), M7 code (`knxide` CLI; real-hardware test still pending), M8 (routing; multicast test is `--ignored`, see tests/routing.rs), M9 (tunnelling over TCP), M10 (ETS CSV/XML import, `knxide-project`; `.knxproj` not yet). M11 (KNX Secure) deferred: no Secure hardware or keyring to test against; plan kept in [docs/m11-secure-plan.md](docs/m11-secure-plan.md).
- Experimental, outside the milestones (branch `device-programming`): application download. `knxide-program` reproduces three recorded downloads request for request (one per mask 0021, 0701, 0705) with every request generated from rules; `apps/knxide-programmer` drives it over a bus. All tests run against simulated devices; **no real device has been downloaded with it yet**. Rules come from behaviour notes kept outside the repository: describe behaviour plainly in code and docs, mark it "verified against a recorded download" or "unverified", and never copy recordings, product data or project data into the repository.
- Test hardware: a KNX IP interface (tunnelling, no Secure).

## Crates

| Crate | Role |
|---|---|
| `knxide` | facade: re-exports core, ip, tokio, project (features `tokio`, `project`) |
| `knxide-core` | addresses, cEMI, TPCI/APCI, DPT codecs. `no_std`, no I/O |
| `knxide-ip` | KNXnet/IP frames + sans-IO state machines. No I/O, no async |
| `knxide-tokio` | async adapter, owns sockets and timers |
| `knxide-cli` | `knxide` binary for validating against real hardware |
| `knxide-project` | ETS group address export import (CSV, XML) |
| `knxide-program` | application downloads for masks 0021/0701/0705, without I/O: project archive reader (`.knxproj`, own ZIP/inflate), inventory with obstacles per device, memory image, communication tables, the ordered bus requests (opening, pre-check, load procedure), request ↔ APDU encoding (`wire`), transport layer connection as a state machine (`transport`); for the editor: edits over a project device (`edit`), what a person sees of it (`view`: pages, parameters, objects, re-evaluated after every edit) and reading a comparison back into parameters and links (`compare`). Experimental, `publish = false`, not in the facade. Replay tests against private recordings run only with `KNXIDE_PROGRAM_FIXTURES=<dir>` |
| `knxide-testkit` | fake gateway (with pluggable simulated devices), a made-up project with three application programs, a stored-only ZIP writer; `publish = false`; `examples/demo_gateway.rs` plays fake traffic, `examples/demo_project.rs` writes the made-up `.knxproj` |
| `apps/knxide-monitor` | web console (axum + WebSocket, page in `static/`), `publish = false` |
| `apps/knxide-dashboard` | customisable dashboard (tiles bound to group addresses, `dashboard.json`), `publish = false` |
| `apps/knxide-proxy` | tunnelling proxy: one upstream tunnel shared by many clients; server side is `knxide_ip::server` |
| `apps/knxide-mcp` | MCP server, stdio or Streamable HTTP (`--listen`, bearer `KNXIDE_MCP_TOKEN`); tunnelling or `--routing`; hand-rolled JSON-RPC, no SDK; writes only with `--allow-write`; `Dockerfile` builds from repo root; `publish = false` |
| `apps/knxide-programmer` | web page that edits and programs devices from ETS project exports (axum + WebSocket, page in `static/`, JSON API in its README). The page starts with a list of projects (`workspace`: a library directory, bring in / open / copy / rename / remove, export as `.knxproj` with the edits written into `0.xml` by `export`, unverified against ETS) and shows a project as things, not devices: `static/model.js` works out lights, outlets and heating from runs of group addresses, the outputs that drive them and the wall plates that control them; rooms with wall plates wired to things, a cabinet view by output, loose ends, a panel for the selected thing / rocker / output / device (parameter pages, group object links), pending changes programmed together through a queue of ordinary downloads; compare (read-only, differences named as parameters and links; survives a lossy link), download (recovers from single failed exchanges by reading what the device holds, never by replaying a write; the tool's own rule, unverified on a real device); routing picks a wired interface and warns on Wi-Fi; edits live in a sidecar file beside the project (`<project>.knxide.json`), the project is never written; floor plans and group address management remain in the API without a view; tunnelling, routing or `--simulate`; no login, listens on localhost by default; `publish = false` |
| `desktop/` (`knxide-desktop`, binary `knxide`) | the programmer as a desktop application: the same server inside the program on a loopback port the system picks, the page in a Tauri 2 window (system web view). Adds only what a window needs: export through the system's file chooser, a question before closing during an operation, one running copy; no bus option given means routing with the interface chosen per operation. **Outside the workspace** like `fuzz/` (own `Cargo.lock`; GTK/WebKitGTK and Tauri's dependency tree stay out of the workspace checks and `deny.toml`); `desktop/install.sh` installs it for the user on Linux; only Linux has been run; `publish = false` |

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

The desktop application is outside the workspace and has its own checks: `cd desktop && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`. Start-up (options, bus, list of projects) is shared with the server through `knxide_programmer::startup`.

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
