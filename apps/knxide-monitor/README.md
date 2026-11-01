# knxide-monitor

A web console for a KNX bus, built on the knxide crates.

- **Bus monitor**: every telegram live, decoded with names and units from your ETS export.
  Filter by text or service, pause, see bus load at a glance.
- **Group addresses**: your ETS project as a tree, each address with its last value, live.
- **Devices** (second tab): every physical address heard on the bus, by line, with frame count,
  last seen and the group addresses it sends to. A device's details have a tucked-away
  *Restart device…* (basic restart like ETS, keeps the configuration), with a confirmation.
- **Health** (third tab), in two sections, with ⚠ marks in place as well:
  - *Bus health* (warnings, counted on the tab): sensors repeating the same value for hours,
    sensors reporting invalid data, devices that go quiet, repeated frames (missing
    acknowledgements), dropped connections and failed writes.
  - *Project* (notes): addresses whose values do not match their ETS DPT, and addresses not
    in the project. For integer meters that really send floats, **Use DPT 14 here** sets a
    DPT override with one click.
- **DPT overrides** are saved next to the project as `<project>.overrides.json` (for
  `knx.xml`: `knx.overrides.json`) and shown as such, with a reset, in the send panel. The
  ETS export itself is never changed; fixing the DPT in ETS is the lasting fix.
- **Send**: write or read any group address. Controls follow the DPT: on/off buttons, dim
  up/down, sliders for percentages, colour pickers, time and date inputs, or raw values.
- Drop an ETS group address export (XML or CSV) on the page to load it.

```sh
cargo install --path apps/knxide-monitor --root ~/.local
knxide-monitor --gateway 192.168.1.50 --project knx.xml
# then open http://127.0.0.1:8080
```

Options: `--listen IP:PORT` (default `127.0.0.1:8080`), `--gateway IP[:PORT]` (default
`$KNXIDE_GATEWAY`), `--tcp`, `--project FILE` (default `./knx.xml`), `--connect` (connect at
start-up), `--stuck-after HOURS` (health: stuck-value threshold, default 6).

The server keeps one KNX connection shared by every open tab, so it uses a single tunnel.

**Security**: whoever can open the page can write to your bus. The server listens on
localhost by default and refuses WebSocket connections from other websites' pages. Only use
`--listen 0.0.0.0:…` on a network you trust.

## Developing without a bus

```sh
cargo run -p knxide-testkit --example demo_gateway -- --listen 127.0.0.1:13671 --project knx.xml
cargo run -p knxide-monitor -- --gateway 127.0.0.1:13671 --project knx.xml --connect --listen 127.0.0.1:8099
```

The demo gateway plays plausible traffic for every address in the project and answers reads.
