# KNX Rust Library — Handoff Brief

> Research and planning brief for implementing a KNX communication library in Rust.
> Intended to be dropped into the repo (e.g. `docs/knx-library-brief.md`) and referenced from `CLAUDE.md`.
> Prepared November 2026. Protocol constants below are summarised from the KNX spec and reference
> implementations; **verify every constant against the spec PDFs before relying on it** (marked ⚠ where extra care is needed).

---

## 1. Goal and scope

Build a **Rust library** that lets a Linux program read from and write to a KNX bus through a KNX IP interface or router. An application will be built on top of it later — this brief covers the library only.

### In scope (v1)
- KNX addressing: individual addresses (`1.1.240`) and group addresses (3-level `1/2/3`, 2-level `1/515`, free 16-bit).
- cEMI `L_Data` frames with group services: `GroupValue_Read`, `GroupValue_Response`, `GroupValue_Write`.
- Datapoint type (DPT) codecs for a core set (see §5).
- KNXnet/IP over UDP: discovery (search/description), **tunnelling client**, heartbeat, reconnect.
- An async (tokio) adapter with a small, ergonomic API.
- A minimal example CLI (`discover`, `monitor`, `read`, `write`) used to validate against real hardware.

### Later milestones (design for them, don't build yet)
- KNXnet/IP routing (multicast).
- Tunnelling v2 over TCP.
- KNX IP Secure and KNX Data Secure.
- Group-address → DPT import from ETS (CSV/XML export, then `.knxproj`).
- Non-IP transports (USB HID, FT1.2, TP-UART). On Linux these can be bridged to KNXnet/IP via `knxd` instead.

### Non-goals
- Acting as a KNX device programmable by ETS (device stack, System B).
- Device management / property access beyond what discovery needs.
- Windows/macOS-specific work (should still compile, but Linux is the target).

---

## 2. Key decisions already made

| Decision | Choice | Reason |
|---|---|---|
| Language | Rust | Owner's choice |
| Architecture | **Sans-IO protocol core + thin tokio adapter** | State machines testable with fake time and no network; runtime-agnostic; same pattern as `quinn-proto` and the `knx-rust` crate |
| Own crate vs adopt existing | **Build our own**, use existing crates as references | All Rust KNX crates are 0.x; none mature enough to depend on (see §8) |
| License | MIT OR Apache-2.0 (Rust convention) | Permissive; see license hygiene in §9 |
| First transport | KNXnet/IP tunnelling over UDP | Works with every KNX IP interface and router |
| Test hardware | **KNX IP interface** (not a router) | Owner's choice. Tunnelling is the only mode it supports, so routing (M8) stays a later milestone and can't be tested on real hardware without a router |
| Crate names | **`knxide-*`**: `knxide-core`, `knxide-ip`, `knxide-tokio`, `knxide-cli` (binary `knxide`), `knxide-testkit` (`publish = false`) | All free on crates.io as of 2026-11-01 (`knx-core` and `knx-ip` are taken). KNX + oxide, after the Rust naming habit (cf. gitoxide). Renamed from `konnex-*` on 2026-11-01 to keep KNX in the name. Reserve the bare `knxide` name for a future facade crate that re-exports the others |
| Edition / MSRV | Edition 2024; `rust-version` = the stable release current at M0 minus ~3, raised deliberately | Owner has no preference |

### Resolved later
- No Secure-enabled hardware or ETS keyring is available for testing, so M11 (KNX Secure) is deferred (2026-11-01).

---

## 3. Protocol primer

KNX on twisted pair (TP1, 9600 baud) is reached from Linux through an interface. The library speaks **KNXnet/IP** to that interface. Three nested layers:

```
UDP datagram (port 3671)
└── KNXnet/IP frame          header + service-specific body (connection header, HPAI, …)
    └── cEMI frame           message code, control fields, src/dst address
        └── TPDU/APDU        TPCI + APCI (read/response/write) + data
            └── DPT-encoded value
```

### 3.1 KNXnet/IP frame header (6 bytes)

| Offset | Size | Field | Value |
|---|---|---|---|
| 0 | 1 | Header length | `0x06` |
| 1 | 1 | Protocol version | `0x10` |
| 2 | 2 | Service type identifier (BE) | see table |
| 4 | 2 | Total length incl. header (BE) | |

Service types (⚠ verify):

| Service | Code |
|---|---|
| SEARCH_REQUEST / RESPONSE | `0x0201` / `0x0202` |
| DESCRIPTION_REQUEST / RESPONSE | `0x0203` / `0x0204` |
| CONNECT_REQUEST / RESPONSE | `0x0205` / `0x0206` |
| CONNECTIONSTATE_REQUEST / RESPONSE (heartbeat) | `0x0207` / `0x0208` |
| DISCONNECT_REQUEST / RESPONSE | `0x0209` / `0x020A` |
| SEARCH_REQUEST_EXTENDED / RESPONSE_EXTENDED | `0x020B` / `0x020C` |
| TUNNELLING_REQUEST / TUNNELLING_ACK | `0x0420` / `0x0421` |
| ROUTING_INDICATION | `0x0530` |
| ROUTING_LOST_MESSAGE / ROUTING_BUSY | `0x0531` / `0x0532` |
| SECURE_WRAPPER, SESSION_* , TIMER_NOTIFY (later) | `0x0950`–`0x0955` |

### 3.2 Common structures
- **HPAI** (host protocol address info, 8 bytes): length `0x08`, protocol `0x01` (IPv4 UDP) / `0x02` (IPv4 TCP), IPv4 address, port.
  - **Route-back / NAT mode**: send `0.0.0.0:0`; the server replies to the UDP source address. Prefer this by default — it survives NAT and Docker.
- **CRI** (connect request info) for tunnelling: length `0x04`, connection type `0x04` (TUNNEL_CONNECTION), KNX layer `0x02` (link layer; `0x80` = busmonitor), reserved `0x00`.
- **CRD** (connect response data): length `0x04`, connection type `0x04`, then the **individual address assigned to this tunnel**.
- **Connection header** (in TUNNELLING_REQUEST/ACK): length `0x04`, channel ID, sequence counter, status/reserved.
- **DIBs** in search/description responses: Device Info (`0x01`), Supported Service Families (`0x02`), plus others to skip gracefully.

Connect response status codes (⚠ verify): `0x00` OK, `0x22` connection type unsupported, `0x23` connection option unsupported, `0x24` **no more connections**, `0x29` tunnelling layer unsupported. Other errors: `0x21` wrong connection ID, `0x26` data connection error, `0x27` KNX connection error, `0x04` sequence number error.

### 3.3 Tunnelling state machine (the hard part)

1. **Connect**: CONNECT_REQUEST (control HPAI, data HPAI, CRI). The response gives a channel ID, the server's data endpoint and the CRD (our individual address). Timeout ≈10 s.
2. **Heartbeat**: CONNECTIONSTATE_REQUEST every **60 s** on the control endpoint. Response timeout ≈10 s; after **3** failed attempts, treat as disconnected. The server drops connections idle for **120 s**.
3. **Sending**: TUNNELLING_REQUEST with our send sequence counter (u8, wrapping). Expect TUNNELLING_ACK with the same sequence within **1 s**; resend **once**. If the second attempt fails, tear down and reconnect.
4. **Receiving**: for each inbound TUNNELLING_REQUEST, always reply with TUNNELLING_ACK.
   - Sequence == expected → process, increment expected.
   - Sequence == expected − 1 → duplicate: ACK again, discard.
   - Otherwise → discard (no ACK).
5. **L_Data.con**: after the server ACKs our `L_Data.req`, it later sends an `L_Data.con` (as a TUNNELLING_REQUEST, which we ACK). The control-field confirm bit indicates success or failure on the bus. **Keep at most one outstanding `L_Data.req` until its `.con` arrives** (timeout ≈3 s) — this is what xknx does and avoids overrunning the interface.
6. **Disconnect**: DISCONNECT_REQUEST in either direction; always respond to a server-initiated one and then reconnect with back-off.
7. **Source address**: send `L_Data.req` with source `0.0.0`; the interface substitutes the tunnel's address.
8. Our own frames come back as `L_Data.con`, not `L_Data.ind`. If the application needs a local echo, the library must synthesise it.

All timer values are in the Tunnelling part of the spec (3/8/4); make them configurable with spec defaults.

### 3.4 Routing (later milestone)
- Connectionless multicast to **224.0.23.12:3671**, ROUTING_INDICATION carrying cEMI `L_Data.ind`. Only KNX IP *routers* participate.
- Honour **ROUTING_BUSY** (contains a wait time) and rate-limit sending (≈50 telegrams/s is the commonly used ceiling).
- Must be able to pick the network interface for multicast; ignore our own looped-back packets.

### 3.5 Tunnelling v2 over TCP (later milestone)
- Same frames over a TCP stream, framed by the header's total-length field.
- **No TUNNELLING_ACK over TCP** (TCP provides reliability).
- Required for KNX IP Secure tunnelling.

### 3.6 cEMI L_Data frame

```
msg code | add-info len | add-info… | ctrl1 | ctrl2 | src (2) | dst (2) | len | TPCI/APCI… | data…
```

- Message codes (⚠ verify): `L_Data.req 0x11`, `L_Data.con 0x2E`, `L_Data.ind 0x29`, `L_Busmon.ind 0x2B`. Property services (`M_PropRead.req 0xFC`, …) are out of scope.
- Additional info: usually length 0; must be parsed and skipped (busmonitor and RF use it).
- **ctrl1** (typical `0xBC`): bit7 frame type (1 = standard), bit5 repeat, bit4 broadcast, bits3–2 priority (`11` = low), bit1 ack request, bit0 confirm (in `.con`: 1 = **error**).
- **ctrl2** (typical `0xE0`): bit7 destination address type (1 = group), bits6–4 hop count (6), bits3–0 extended frame format.
- **len** = number of octets after the first TPCI/APCI octet.

### 3.7 Addresses
- Individual: `area(4 bits).line(4).device(8)` → `1.1.240`.
- Group, 16 bits: 3-level `main(5)/middle(3)/sub(8)` → `1/2/3`; 2-level `main(5)/sub(11)`; or free `0..=65535`. Display style is presentation only; the wire value is always a u16.

### 3.8 TPCI/APCI for group services
Two octets: TPCI (6 bits) plus the 10-bit APCI split across them.

| Service | APCI | Bytes (no data) |
|---|---|---|
| GroupValue_Read | `0x000` | `0x00 0x00` |
| GroupValue_Response | `0x040` | `0x00 0x40` |
| GroupValue_Write | `0x080` | `0x00 0x80` |

- **Short encoding**: values of ≤6 bits (DPT 1, 2, 3) go in the low 6 bits of the second octet (e.g. switch-on write = `0x00 0x81`, len = 1).
- Longer values are appended after the two octets (e.g. DPT 9 write = `0x00 0x80 HH LL`, len = 3).
- ⚠ Only short-encode values that actually fit in 6 bits. A one-byte DPT 5 value must be appended, never short-encoded, even if it is small. Reference implementations have had bugs here.

---

## 4. Specification sources

The KNX Specifications are **free** after creating an account at MyKNX and "ordering" them for €0: <https://my.knx.org/en/shop/knx-specifications>. The current public version is **v3 (Feb 2025)**; v2.1 PDFs also circulate.

Relevant parts (v2.1 file numbering shown; v3 keeps the Volume/Part structure — ⚠ verify names):

| Part | Topic | Needed for |
|---|---|---|
| 3/8/1 | KNXnet/IP Overview | Orientation |
| 3/8/2 | KNXnet/IP **Core** | Header, HPAI, DIBs, search/describe/connect/heartbeat |
| 3/8/4 | KNXnet/IP **Tunnelling** | Tunnel state machine, timers, sequence rules |
| 3/8/5 | KNXnet/IP **Routing** | Multicast, ROUTING_BUSY, flow control |
| 3/6/3 | EMI/IMI (**cEMI**) | cEMI frame format, message codes |
| 3/3/7 | **Application Layer** | APCI codes |
| 3/3/4 | Transport Layer | TPCI |
| 3/7/2 | **Datapoint Types** | DPT encodings |
| AN159 / AN158 (v2.1) or the Secure parts of Vol. 3 (v3) | KNXnet/IP Secure / Data Secure | Later milestone |

Formal equivalents: ISO 22510 / EN 13321-2 (KNXnet/IP). EN 13321-2 does not cover cEMI or DPTs.

A publicly posted copy of the v2.1 Core spec, useful for a quick look: <https://github.com/dufourgilles/knx-ip/files/5081907/KNX.spec.03_08_02.Core.v01.05.01.AS.pdf>

**Code convention**: doc-comment every protocol type and constant with its spec reference (part and section).

---

## 5. Datapoint types (v1 set)

The bus carries **no type information**. The application must know each group address's DPT (from config or ETS import). The library provides typed codecs plus a dynamic `DptValue` enum keyed by `(main, sub)`.

| DPT | Size | Encoding | Examples |
|---|---|---|---|
| 1.xxx | 1 bit (short) | bool | 1.001 switch, 1.008 up/down, 1.009 open/close |
| 2.xxx | 2 bits (short) | control + value | 2.001 switch control |
| 3.007 / 3.008 | 4 bits (short) | direction bit + 3-bit step code | dimming, blinds |
| 5.001 | 1 byte | u8 scaled 0..255 ↔ 0..100 % | percentage |
| 5.003 | 1 byte | u8 scaled ↔ 0..360° | angle |
| 5.010 | 1 byte | raw u8 | counter |
| 6.xxx | 1 byte | i8 | |
| 7.xxx / 8.xxx | 2 bytes | u16 / i16 | |
| **9.xxx** | 2 bytes | **KNX 2-byte float** | 9.001 temperature, 9.004 lux, 9.007 humidity |
| 10.001 | 3 bytes | time of day + weekday | |
| 11.001 | 3 bytes | date | |
| 12.xxx / 13.xxx | 4 bytes | u32 / i32 | |
| 14.xxx | 4 bytes | IEEE 754 f32 | power, energy |
| 16.000 / 16.001 | 14 bytes | ASCII / ISO-8859-1 string, zero-padded | |
| 17.001 / 18.001 | 1 byte | scene number / scene control | |
| 19.001 | 8 bytes | date-time | later |
| 20.xxx | 1 byte | enums (HVAC mode, …) | add as needed |
| 232.600 | 3 bytes | RGB | add as needed |

**DPT 9 (2-byte float)**: bits `MEEEEMMM MMMMMMMM`. Value = `0.01 × M × 2^E`, where M is a 12-bit two's-complement mantissa (sign bit + 11 bits) and E is a 4-bit exponent. Encode by choosing the smallest E such that M fits in −2048..2047. `0x7FFF` = invalid, so the largest value is `0x7FFE`. Range −671 088.64 … 670 433.28 (corrected from 670 760.96 during M2; matches xknx). Property-test that the round-trip error is within the quantisation step.

**Idea worth stealing from `knxkit`**: generate DPT/DPST metadata (names, units, ranges) at build time from ETS's `knx_master.xml` instead of hand-writing it. Optional; hand-written codecs for the v1 set are fine.

---

## 6. Library architecture

Cargo workspace:

```
crates/
  knxide-core/     addresses, cEMI, TPCI/APCI, DPT codecs        no I/O, #![no_std] + alloc if practical
  knxide-ip/       KNXnet/IP frames + sans-IO state machines     no I/O, no async
                     - discovery (search/describe)
                     - tunnel client (UDP; TCP later)
                     - router (later)
  knxide-tokio/    async adapter: owns sockets + timers, exposes the public async API
  knxide-cli/      example/validation binary (discover, monitor, read, write)
  knxide-testkit/  fake KNXnet/IP gateway built on knxide-ip, fixtures, helpers (dev-only)
# later: knxide-secure (feature-gated), knxide-project (ETS import), knxide-serial/usb
```

### 6.1 Sans-IO core API shape (knxide-ip)
Model it on `quinn-proto`. The state machine never touches sockets or clocks; time is passed in.

```rust
let mut tunnel = TunnelClient::new(config, now);
tunnel.handle_input(remote_addr, &datagram, now);   // feed inbound bytes
while let Some(tx) = tunnel.poll_transmit() { /* send tx.payload to tx.destination */ }
if let Some(deadline) = tunnel.poll_timeout() { /* sleep until deadline */ }
tunnel.handle_timeout(now);                          // drive heartbeat / ack / con timers
while let Some(ev) = tunnel.poll_event() { /* Connected, Disconnected, Frame(cemi), SendResult(id, Ok|Err) */ }
let id = tunnel.send(cemi_frame)?;                   // queue L_Data.req; result arrives as an event
```

Rules:
- `now` is a caller-supplied `Instant`-like value. Tests use a fake clock.
- All decoding returns `Result`; **no panics on malformed input**, ever. Unknown DIBs and services are skipped or ignored, not errors.
- The internal send queue enforces the "one outstanding `L_Data.req` until `.con`" rule.

### 6.2 Async API shape (knxide-tokio)
Draft only — refine during implementation:

```rust
let client = TunnelClient::connect("192.168.1.50:3671".parse()?, TunnelOptions::default()).await?;
client.group_write(ga!(1/2/3), Dpt9(21.5)).await?;          // resolves on L_Data.con
let value = client.group_read(ga!(1/2/4), Duration::from_secs(2)).await?;  // waits for GroupValue_Response
let mut events = client.subscribe();                          // broadcast of inbound group telegrams + connection events
let gateways = discover(Duration::from_secs(3)).await?;
```

- Automatic reconnect with exponential back-off; surface connection state as events.
- Cloneable handle (`Arc` + mpsc to a single driver task owning the socket).
- Options: local bind address and interface, NAT/route-back mode (default on), all spec timers.
- Use `tracing` for logs. Don't leak tokio types from `knxide-core` or `knxide-ip`.

### 6.3 Dependencies policy
Keep it small: `thiserror`, `tracing`, `tokio` (adapter only), `proptest` (dev). Ask before adding anything else. Crypto crates only arrive with `knxide-secure`.

Added during implementation: `socket2` in `knxide-tokio` (already in the tree through tokio; needed for the multicast interface and port sharing in routing), and `libfuzzer-sys` in the out-of-workspace `fuzz/` crate.

---

## 7. Milestones and acceptance criteria

| # | Milestone | Acceptance |
|---|---|---|
| M0 | Workspace, CI (`fmt`, `clippy -D warnings`, tests, MSRV check, **`cargo-deny` for licenses and advisories**) | CI green on an empty workspace |
| M1 | `knxide-core` addresses + cEMI `L_Data` + group APCI encode/decode | Proptest round-trips; golden byte fixtures from spec examples; parse/display for all address formats |
| M2 | DPT codecs (§5 v1 set) | Round-trip proptests; boundary and invalid values; DPT 9 fixtures |
| M3 | `knxide-ip` frame types: header, HPAI, CRI/CRD, DIBs, all core and tunnelling services | Golden fixtures (incl. real Wireshark captures); fuzz targets for every decoder (`cargo-fuzz`) run in CI for a short time |
| M4 | Sans-IO tunnel state machine + discovery | Fake-clock unit tests for: connect OK and each error status; heartbeat success and 3× failure; ACK timeout → resend → teardown; duplicate and out-of-order sequence; server-initiated disconnect; negative `L_Data.con`; `.con` timeout; sequence counter wraparound |
| M5 | `knxide-testkit` fake gateway | Integration tests run tunnel + fake gateway over loopback UDP |
| M6 | `knxide-tokio` adapter | Integration tests against the fake gateway, incl. reconnect after gateway restart |
| M7 | `knxide-cli` (`discover`, `monitor`, `read <ga> --dpt`, `write <ga> <value> --dpt`) | Manual test against real hardware: discovery, monitoring live traffic, reading a sensor, switching an actuator |
| M8 | Routing | Fake-router tests incl. ROUTING_BUSY; real router test if hardware is available |
| M9 | Tunnelling over TCP | Fake gateway TCP mode |
| M10 | GA→DPT import: ETS group-address CSV/XML export first, `.knxproj` later | Parses a sample export into a typed GA table |
| M11 | KNX IP Secure, then Data Secure (`knxide-secure`, feature-gated) | Separate plan before starting |

Stop and check in with the owner after M4 (API shape review) and M7 (real-hardware validation).

---

## 8. Reference implementations

### Rust (all 0.x — references, not dependencies)
| Crate | License | Why look |
|---|---|---|
| **knust** — <https://github.com/dphi/knust> · <https://docs.rs/knust> | MIT | Most complete async (tokio) client: tunnelling, routing, discovery, optional IP Secure (verified on hardware), experimental Data Secure, `.knxkeys` parsing, ETS CSV import, tunnelling server feature (usable as a test gateway) |
| **knx-rust** — <https://docs.rs/crate/knx-rust/latest> | check | **Sans-IO design reference**: no I/O; caller feeds inbound data, drains outbound, drives timers. Examples for tokio and mio |
| **knxkit** — <https://crates.io/crates/knxkit> | check | `.knxproj` parsing; DPT definitions generated from `knx_master.xml` |
| **knx-rs** (metaneutrons) — <https://github.com/metaneutrons/knx-rs> · <https://docs.rs/knx_rs_ip> | **GPL-3.0-only** | Active 2026 workspace: tunnel with retry/heartbeat/auto-reconnect, rate-limited routing, discovery, connection multiplexer. **Read only, never copy** |
| **knx-pico** — <https://crates.io/crates/knx-pico> | check | `no_std` tunnelling for Embassy; useful if `knxide-core` should be embedded-friendly |
| mabi-knx — <https://lib.rs/crates/mabi-knx> | check | KNXnet/IP simulator; possible test gateway |

### Other languages
| Project | License | Why look |
|---|---|---|
| **xknx** (Python) — <https://github.com/XKNX/xknx> | MIT | **Best behavioural reference.** Powers Home Assistant's KNX integration; robust tunnelling, reconnect, IP Secure, Data Secure |
| xknxproject (Python) — <https://github.com/XKNX/xknxproject> | GPL-2.0 ⚠ verify | `.knxproj` parsing (ETS 4–6, password-protected) |
| knx-go (Go) — <https://github.com/vapourismo/knx-go> | MIT | Clean, small reference for KNXnet/IP, cEMI and DPT packages. **No longer maintained**; no Secure |
| KNXUltimate (TypeScript) — <https://github.com/Supergiovane/KNXUltimate> | MIT | Active; UDP/TCP tunnelling, routing, IP Secure, Data Secure, FT1.2 serial |
| knx.js (Node) — <https://github.com/ekarak/knx> | MIT | Historical; npm `knx` 2.5.4, stale |
| Calimero (Java) — <https://github.com/calimero-project/calimero-core> | **GPL** | Most comprehensive (KNXnet/IP, IP Secure, USB, RF USB, FT1.2, TP-UART). Read only |
| knxd (C++) — <https://github.com/knxd/knxd> | GPL | Linux daemon that talks to all common interfaces and exposes KNXnet/IP. Use it to bridge USB/TP-UART hardware to our library rather than implementing those transports |

### Tooling
- **Wireshark** has a built-in KNXnet/IP dissector. Capture real traffic for fixtures and debugging.
- **Scapy** `scapy.contrib.knx` for crafting test packets: <https://scapy.readthedocs.io/en/latest/api/scapy.contrib.knx.html>

---

## 9. Rules for the implementing session

1. **License hygiene**: the library is MIT OR Apache-2.0. GPL projects (Calimero, knx-rs, knxd, possibly xknxproject) may be read to understand behaviour, but **no code is copied or closely translated** from them. MIT sources may be borrowed from with attribution in a `NOTICE` or code comment. Enforce dependency licenses with `cargo-deny`.
2. **Spec first**: implement from the spec. Use reference libraries to resolve ambiguities and real-world quirks, and note such quirks in comments (e.g. "interface X sends Y; see xknx …").
3. Every protocol constant and struct gets a spec-reference doc comment.
4. Decoders never panic; fuzz them.
5. Keep `knxide-core` and `knxide-ip` free of I/O, async and global state.
6. Make timers and limits configurable with spec defaults.
7. Don't implement Secure, routing, TCP or ETS import before their milestones. Do keep the transport abstraction ready for TCP and the Secure wrapper.
8. Check in with the owner at the M4 and M7 gates, and before adding dependencies.

---

## 10. Glossary

- **cEMI**: common External Message Interface — the frame format inside KNXnet/IP.
- **DPT / DPST**: Datapoint Type / Subtype, e.g. 9.001 = temperature °C.
- **GA**: group address — the "function" address that actuators and sensors share.
- **IA**: individual address — physical device address.
- **ETS**: Engineering Tool Software — the official KNX configuration tool. Its project contains the GA → DPT mapping.
- **HPAI / CRI / CRD / DIB**: KNXnet/IP structures for endpoints, connect request, connect response, and device information.
- **Tunnelling / Routing**: unicast point-to-point vs multicast KNXnet/IP modes.
- **KNX IP Secure / Data Secure**: transport-level session encryption vs end-to-end per-GA encryption. Keys come from the ETS keyring (`.knxkeys`).
