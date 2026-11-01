# knxide

A Rust library for talking to a KNX bus over KNXnet/IP. It also comes with a command-line
tool and three small web apps.

Written from the KNX specification. No unsafe code, and the decoders are fuzzed.

## What it supports

- Tunnelling over UDP and TCP
- Routing (multicast)
- Gateway discovery
- Reading and writing group addresses
- Datapoint types (DPTs): switches, percentages, temperatures, floats, times, dates, text,
  HVAC modes, colours and more
- Importing group addresses from an ETS export (CSV or XML)
- Restarting a device, like "Restart device" in ETS

Not supported yet: KNX Secure, USB and serial interfaces, and `.knxproj` files.

## Command line

```sh
cargo install knxide-cli

knxide discover                                # find gateways on the network
knxide monitor --gateway 192.168.1.50          # show bus traffic
knxide read 1/2/4 --dpt 9.001                  # read a temperature
knxide write 1/2/3 on --dpt 1.001              # turn something on
```

Give it your ETS export with `--project knx.xml` and it shows names and decodes values for
you. Set `KNXIDE_GATEWAY` to skip `--gateway`. Run `knxide` without arguments for all
options.

## Library

```toml
[dependencies]
knxide-core = "0.1"
knxide-tokio = "0.1"
tokio = { version = "1", features = ["macros", "rt"] }
```

```rust
use std::time::Duration;
use knxide_core::{dpt, ga};
use knxide_tokio::{Client, TunnelOptions};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::connect("192.168.1.50:3671".parse()?, TunnelOptions::default()).await?;
    client.group_write(ga!(1/2/3), dpt::dpt1::encode(true)).await?;
    let raw = client.group_read(ga!(1/2/4), Duration::from_secs(2)).await?;
    println!("temperature: {}", dpt::dpt9::decode(&raw)?);
    Ok(())
}
```

## Crates

| Crate | What it is |
|---|---|
| `knxide-core` | Addresses, telegrams and DPTs. No I/O, works without `std` |
| `knxide-ip` | KNXnet/IP frames and connection logic. No I/O, so you can use any runtime |
| `knxide-tokio` | Ready-to-use async client on top of tokio |
| `knxide-project` | Reads ETS group address exports |
| `knxide-cli` | The `knxide` command |
| `knxide-testkit` | A fake gateway for tests |

## Apps

Each app has its own README.

- [knxide-monitor](apps/knxide-monitor): a web page that shows live bus traffic, lets you
  send values, lists devices, and points out problems such as values that stopped changing.
- [knxide-dashboard](apps/knxide-dashboard): a dashboard for phone, tablet or desktop with
  lights, temperatures, blinds and scenes. You set it up in the browser.
- [knxide-proxy](apps/knxide-proxy): shares one connection to your gateway with many
  programs, since most gateways only allow a few at a time.

To try the apps without a real bus, start the fake gateway and point an app at it:

```sh
cargo run -p knxide-testkit --example demo_gateway -- --project knx.xml   # --project is optional
cargo run -p knxide-dashboard -- --gateway 127.0.0.1
```

## Building

Needs Rust 1.96 or newer.

```sh
cargo test --workspace
```

## License

MIT or Apache-2.0, at your choice.
