# M11 plan: KNX IP Secure and KNX Data Secure

Status: **deferred** (2026-11-01): no Secure-enabled hardware or ETS keyring is available for testing. Kept for when Secure is needed.

Original status: draft for owner review. The brief requires a separate plan before M11 starts, and
the work below needs decisions only the owner can make (see "Decisions needed").

## Do we need it?

Only if the installation uses it:

- **KNX IP Secure** protects the KNXnet/IP link (tunnelling over TCP, or routing). An IP
  interface configured as Secure in ETS refuses plain connections. An interface that is
  not Secure needs none of this.
- **KNX Data Secure** encrypts individual group addresses end to end on the bus. Only matters
  if some group addresses are marked Secure in ETS.

Check in ETS: interface properties → "Secure tunnelling", and the group address "Security"
column (the CSV export from M10 has it).

## Scope, in order

| Step | Content | Acceptance |
|---|---|---|
| S1 | Crypto primitives: AES-128 CBC-MAC + CTR ("CCM" as KNX uses it), X25519 ECDH, PBKDF2-HMAC-SHA256 key derivation | Test vectors from the spec examples (AN159 / Vol. 3 Secure parts) and xknx's tests (MIT) |
| S2 | ETS keyring (`.knxkeys`) import: XML, password hash + AES-128-CBC encrypted keys | Parses a sample keyring; wrong password detected |
| S3 | Sans-IO secure session (`knxide-secure`): `SESSION_REQUEST/RESPONSE`, `SESSION_AUTHENTICATE`, `SESSION_STATUS`, `SECURE_WRAPPER` wrap/unwrap, sequence numbers, keepalive | Fake-clock tests incl. bad MAC, replayed sequence number, auth failure, timeout |
| S4 | Wire into the TCP tunnel (M9): secure link between `Link::Tcp` and the tunnel state machine; CLI `--keyring FILE --password …` or `--user-id N --user-password …` | Fake gateway secure mode; then real hardware if the interface is Secure |
| S5 | Secure routing: backbone key, `TIMER_NOTIFY` time sync, multicast `SECURE_WRAPPER` | Fake router tests |
| S6 | Data Secure: `S-A_Data` APDUs, per-address keys and sequence counters from the keyring, persisted sequence numbers | Vectors + fake gateway |

S1–S4 is the useful core for a Secure IP interface. S5 is needed only for Secure routers, and
S6 only for Data Secure group addresses.

## Protocol facts (verified against xknx, to be checked against the spec PDFs)

- Session setup: client sends `SESSION_REQUEST` with its X25519 public key; server answers
  with its key, a session id and a MAC. Shared secret → SHA-256 → first 16 bytes = session
  key. The server's MAC is checked with the device authentication code.
- User password key: PBKDF2-HMAC-SHA256(password, salt `user-password.1.secure.ip.knx.org`,
  65 536 iterations, 16 bytes). Device authentication code: same with salt
  `device-authentication-code.1.secure.ip.knx.org`.
- `SECURE_WRAPPER`: header, session id (2), sequence number (6), serial number (6), message
  tag (2), encrypted frame, MAC (16): 38 octets of overhead. MAC by AES-CBC over the
  associated data and payload; frame and MAC encrypted with AES-CTR, MAC at counter 0.
- Session timeout 60 s; keepalive (`SESSION_STATUS` keepalive) every 50 s.
- Keyring: PBKDF2-HMAC-SHA256(keyring password, salt `1.keyring.ets.knx.org`, 65 536, 16);
  keys inside are AES-128-CBC encrypted.

## Architecture

- New crate `knxide-secure`, sans-IO like `knxide-ip`: `SecureSession` takes plaintext
  KNXnet/IP frames from the tunnel and gives `SECURE_WRAPPER` frames for the link, and the
  reverse. The tunnel state machine stays unaware of security.
- `knxide-tokio` gets a `secure` cargo feature that adds a secure TCP link. Nothing changes
  for users who do not enable it.
- Keys are held in a zeroising type and never logged.

## Dependencies (need approval)

RustCrypto crates, all widely used: `aes`, `ctr`, `cbc`, `sha2`, `hmac`, `pbkdf2`, plus
`x25519-dalek`, and `zeroize`. Licences are MIT OR Apache-2.0, except `x25519-dalek`
(BSD-3-Clause), which `deny.toml` would need to allow. Only pulled in with the `secure`
feature.

## Decisions needed

1. Is the IP interface Secure? If not, defer M11 entirely.
2. Approve the crypto dependencies above (and BSD-3-Clause in `deny.toml`).
3. Will an ETS keyring export (`.knxkeys` + its password) be available for testing?
4. S5 and S6: needed at all for this installation?
