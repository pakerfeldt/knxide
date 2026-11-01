//! `knxide`: discover gateways, monitor the bus, and read or write group addresses.

mod args;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use args::Args;
use knxide_core::dpt::DptId;
use knxide_core::{Apdu, Cemi, GroupAddress, IndividualAddress, LData, Payload};
use knxide_ip::discovery::Gateway;
use knxide_ip::structures::TunnelLayer;
use knxide_ip::tunnel::Transport;
use knxide_project::GroupAddressTable;
use knxide_tokio::{Client, ClientEvent, RoutingOptions, TunnelOptions, describe, discover};

const USAGE: &str = "\
knxide: talk to a KNX bus through a KNXnet/IP interface

USAGE:
    knxide discover [--timeout SECS] [--host IP[:PORT]]
    knxide monitor  [CONNECTION] [--dpt GA=DPT]... [--count N] [--busmonitor]
    knxide read     [CONNECTION] <GA> [--dpt DPT] [--timeout SECS]
    knxide write    [CONNECTION] <GA> <VALUE> [--dpt DPT]
    knxide restart  [CONNECTION] <INDIVIDUAL ADDRESS> [--yes]

CONNECTION (tunnelling by default, or routing through a KNX IP router):
    --gateway IP[:PORT]  Tunnelling server. Default: $KNXIDE_GATEWAY, else the only
                         tunnelling server found by discovery. Port defaults to 3671.
    --tcp                Tunnel over TCP (tunnelling v2) instead of UDP.
    --no-nat             Advertise our local address instead of route-back (NAT) mode.
    --routing            Use KNXnet/IP routing (multicast 224.0.23.12) instead of a tunnel.
    --interface IP       Routing: local interface address for multicast.
    --address IA         Routing: our individual address. Default 15.15.250.

OPTIONS:
    --dpt DPT            Datapoint type, e.g. 1.001, 5.001, 9.001, 14.056, 16.000.
                         For monitor: GA=DPT, repeatable, e.g. --dpt 1/2/3=9.001.
    --timeout SECS       How long to wait (discover: 3, read: 2).
    --project FILE       ETS group address export (CSV or XML) giving each address its
                         DPT and name. --dpt overrides it.
    --count N            monitor: exit after N frames.
    --busmonitor         monitor: open the tunnel in busmonitor mode (raw frames).
    --yes                restart: do not ask for confirmation.
    --host IP[:PORT]     discover: ask this host directly instead of multicast.

VALUES (write):
    With --dpt, the value is parsed for that type: on/off, 21.5, 50, +3, 12:30:00,
    2026-11-01, comfort, #ff8800, text, ...
    Without --dpt: an integer 0-63 is sent as a small (6-bit) value, and 0x... as bytes,
    e.g. 0x0c1a.

EXAMPLES:
    knxide discover
    knxide write --gateway 192.168.1.50 1/2/3 on --dpt 1.001
    knxide read 1/2/4 --dpt 9.001
    knxide monitor --dpt 1/2/4=9.001
    knxide restart 1.1.80

restart sends a basic A_Restart, like ETS's 'Restart device': the device restarts its
application and keeps its configuration (never a master reset).
";

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let mut raw = std::env::args().skip(1);
    let Some(command) = raw.next() else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let args = match Args::parse(raw) {
        Ok(args) => args,
        Err(err) => return usage_error(&err),
    };
    if args.switch("help") || matches!(command.as_str(), "help" | "--help" | "-h") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let result = match command.as_str() {
        "discover" => cmd_discover(&args).await,
        "monitor" => cmd_monitor(&args).await,
        "read" => cmd_read(&args).await,
        "write" => cmd_write(&args).await,
        "restart" => cmd_restart(&args).await,
        other => return usage_error(&format!("unknown command {other:?}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(msg)) => usage_error(&msg),
        Err(Failure::Runtime(msg)) => {
            eprintln!("knxide: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("knxide: {msg}\nRun `knxide help` for usage.");
    ExitCode::from(2)
}

enum Failure {
    Usage(String),
    Runtime(String),
}

impl From<knxide_tokio::Error> for Failure {
    fn from(err: knxide_tokio::Error) -> Self {
        Self::Runtime(err.to_string())
    }
}

impl From<String> for Failure {
    fn from(msg: String) -> Self {
        Self::Usage(msg)
    }
}

fn usage<T>(msg: impl Into<String>) -> Result<T, Failure> {
    Err(Failure::Usage(msg.into()))
}

fn seconds(args: &Args, default: f64) -> Result<Duration, Failure> {
    let Some(text) = args.flag("timeout") else {
        return Ok(Duration::from_secs_f64(default));
    };
    match text.parse::<f64>() {
        Ok(s) if s > 0.0 && s.is_finite() => Ok(Duration::from_secs_f64(s)),
        _ => usage(format!("invalid --timeout {text:?}")),
    }
}

fn parse_endpoint(text: &str) -> Result<SocketAddr, Failure> {
    if let Ok(addr) = text.parse::<SocketAddr>() {
        return Ok(addr);
    }
    match text.parse::<IpAddr>() {
        Ok(ip) => Ok(SocketAddr::new(ip, knxide_ip::frame::KNXNETIP_PORT)),
        Err(_) => usage(format!("invalid address {text:?}, expected IP or IP:PORT")),
    }
}

fn parse_ga(text: &str) -> Result<GroupAddress, Failure> {
    text.parse()
        .or_else(|_| usage(format!("invalid group address {text:?}")))
}

fn parse_dpt(text: &str) -> Result<DptId, Failure> {
    let dpt: DptId = text
        .parse()
        .or_else(|_| usage(format!("invalid DPT {text:?}")))?;
    if !dpt.is_supported() {
        return usage(format!("DPT {dpt} is not supported"));
    }
    Ok(dpt)
}

async fn cmd_discover(args: &Args) -> Result<(), Failure> {
    args.only(&["timeout", "host"])?;
    let timeout = seconds(args, 3.0)?;
    let gateways = match args.flag("host") {
        Some(host) => describe(parse_endpoint(host)?, timeout)
            .await?
            .into_iter()
            .collect(),
        None => discover(timeout).await?,
    };
    if gateways.is_empty() {
        return Err(Failure::Runtime("no KNXnet/IP servers found".into()));
    }
    for gateway in &gateways {
        println!("{}", describe_gateway(gateway));
    }
    Ok(())
}

fn describe_gateway(gateway: &Gateway) -> String {
    let mut out = format!("{}  {}", gateway.control_endpoint, gateway.name());
    if let Some(info) = gateway.device_info() {
        let mac = info.mac_address.map(|b| format!("{b:02x}")).join(":");
        let _ = write!(
            out,
            "\n    address {}  medium {}  MAC {mac}",
            info.address, info.medium
        );
        if info.programming_mode() {
            out.push_str("  (programming mode)");
        }
    }
    let services: Vec<String> = gateway
        .service_families()
        .iter()
        .map(|f| {
            format!(
                "{} v{}",
                f.name()
                    .map_or_else(|| format!("family {:#04x}", f.id), String::from),
                f.version
            )
        })
        .collect();
    let _ = write!(out, "\n    services: {}", services.join(", "));
    out
}

/// The gateway from `--gateway`, `$KNXIDE_GATEWAY`, or discovery.
async fn gateway(args: &Args) -> Result<SocketAddr, Failure> {
    if let Some(text) = args.flag("gateway") {
        return parse_endpoint(text);
    }
    if let Ok(text) = std::env::var("KNXIDE_GATEWAY")
        && !text.is_empty()
    {
        return parse_endpoint(&text);
    }
    let found = discover(Duration::from_secs(2)).await?;
    let tunnelling: Vec<&Gateway> = found.iter().filter(|g| g.supports_tunnelling()).collect();
    match tunnelling.as_slice() {
        [one] => {
            eprintln!("Using {} ({})", one.control_endpoint, one.name());
            Ok(one.control_endpoint)
        }
        [] => Err(Failure::Runtime(
            "no tunnelling server found; pass --gateway".into(),
        )),
        _ => Err(Failure::Runtime(format!(
            "{} tunnelling servers found; pick one with --gateway (see `knxide discover`)",
            tunnelling.len()
        ))),
    }
}

async fn connect(args: &Args, layer: TunnelLayer) -> Result<Client, Failure> {
    if args.switch("routing") {
        if layer == TunnelLayer::Busmonitor {
            return usage("--busmonitor needs a tunnel, not --routing");
        }
        let mut options = RoutingOptions::default();
        if let Some(ip) = args.flag("interface") {
            options.interface = Some(
                ip.parse()
                    .or_else(|_| usage(format!("invalid --interface {ip:?}")))?,
            );
        }
        if let Some(address) = args.flag("address") {
            options.router.individual_address = address
                .parse()
                .or_else(|_| usage(format!("invalid --address {address:?}")))?;
        }
        return Ok(Client::routing(options).await?);
    }
    let gateway = gateway(args).await?;
    let mut options = TunnelOptions {
        nat: !args.switch("no-nat"),
        ..TunnelOptions::default()
    };
    options.tunnel.layer = layer;
    if args.switch("tcp") {
        options.tunnel.transport = Transport::Tcp;
    }
    Ok(Client::connect(gateway, options).await?)
}

/// A value from the command line, as a payload.
fn parse_value(text: &str, dpt: Option<DptId>) -> Result<Payload, Failure> {
    if let Some(dpt) = dpt {
        let value = dpt
            .parse_value(text)
            .or_else(|e| usage(format!("invalid value {text:?} for DPT {dpt}: {e}")))?;
        return dpt
            .encode(&value)
            .or_else(|e| usage(format!("invalid value {text:?} for DPT {dpt}: {e}")));
    }
    Payload::parse_raw(text).map_or_else(
        || {
            usage(format!(
                "without --dpt, VALUE must be 0-63 or 0x-prefixed hex, got {text:?}"
            ))
        },
        Ok,
    )
}

fn format_payload(payload: &Payload) -> String {
    payload.to_string()
}

fn format_value(payload: &Payload, dpt: Option<DptId>) -> String {
    match dpt.map(|d| (d, d.decode(payload))) {
        Some((dpt, Ok(value))) => format!("{}  ({})", dpt.format(&value), format_payload(payload)),
        Some((dpt, Err(err))) => format!("{}  (not a valid {dpt}: {err})", format_payload(payload)),
        None => format_payload(payload),
    }
}

async fn cmd_read(args: &Args) -> Result<(), Failure> {
    args.only(&[
        "gateway",
        "dpt",
        "timeout",
        "no-nat",
        "routing",
        "interface",
        "address",
        "tcp",
        "project",
    ])?;
    let [ga] = args.positional.as_slice() else {
        return usage("read takes one group address");
    };
    let ga = parse_ga(ga)?;
    let types = Types::load(args)?;
    let dpt = types.dpt(ga);
    let timeout = seconds(args, 2.0)?;
    let client = connect(args, TunnelLayer::LinkLayer).await?;
    let result = client.group_read_from(ga, timeout).await;
    client.close().await;
    match result {
        Ok((source, payload)) => {
            println!("{}", format_value(&payload, dpt));
            eprintln!("Response from {source}");
            Ok(())
        }
        Err(knxide_tokio::Error::Timeout) => {
            Err(Failure::Runtime(format!("no response from {ga}")))
        }
        Err(err) => Err(err.into()),
    }
}

async fn cmd_write(args: &Args) -> Result<(), Failure> {
    args.only(&[
        "gateway",
        "dpt",
        "no-nat",
        "routing",
        "interface",
        "address",
        "tcp",
        "project",
    ])?;
    let [ga, value] = args.positional.as_slice() else {
        return usage("write takes a group address and a value");
    };
    let ga = parse_ga(ga)?;
    let types = Types::load(args)?;
    let dpt = types.dpt(ga);
    let payload = parse_value(value, dpt)?;
    let client = connect(args, TunnelLayer::LinkLayer).await?;
    let result = client.group_write(ga, payload).await;
    client.close().await;
    Ok(result?)
}

async fn cmd_restart(args: &Args) -> Result<(), Failure> {
    args.only(&[
        "gateway",
        "no-nat",
        "routing",
        "interface",
        "address",
        "tcp",
        "yes",
    ])?;
    let [device] = args.positional.as_slice() else {
        return usage("restart takes one individual address, e.g. 1.1.80");
    };
    let device: IndividualAddress = device.parse().or_else(|_| {
        usage(format!(
            "invalid individual address {device:?}, expected e.g. 1.1.80"
        ))
    })?;
    if !args.switch("yes") && !confirm(&format!("Restart device {device}?"))? {
        return Err(Failure::Runtime("not restarted".into()));
    }
    let client = connect(args, TunnelLayer::LinkLayer).await?;
    let result = client.restart_device(device).await;
    client.close().await;
    result?;
    eprintln!("Restart sent to {device}.");
    Ok(())
}

/// Ask on the terminal. Without a terminal, refuse rather than act unasked.
fn confirm(question: &str) -> Result<bool, Failure> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return usage("refusing to restart without confirmation; pass --yes");
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|e| Failure::Runtime(e.to_string()))?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}

async fn cmd_monitor(args: &Args) -> Result<(), Failure> {
    args.only(&[
        "gateway",
        "dpt",
        "count",
        "busmonitor",
        "no-nat",
        "routing",
        "interface",
        "address",
        "tcp",
        "project",
    ])?;
    if !args.positional.is_empty() {
        return usage("monitor takes no positional arguments");
    }
    let types = Types::load(args)?;
    let count = match args.flag("count") {
        Some(n) => Some(
            n.parse::<u64>()
                .or_else(|_| usage(format!("invalid --count {n:?}")))?,
        ),
        None => None,
    };
    let layer = if args.switch("busmonitor") {
        TunnelLayer::Busmonitor
    } else {
        TunnelLayer::LinkLayer
    };
    let client = connect(args, layer).await?;
    let mut events = client.subscribe();
    eprintln!(
        "Connected as {}. Press Ctrl-C to stop.",
        client.address().map_or_else(String::new, |a| a.to_string())
    );
    let start = Instant::now();
    let mut seen = 0u64;
    loop {
        let event = tokio::select! {
            event = events.recv() => event,
            _ = tokio::signal::ctrl_c() => break,
        };
        let elapsed = start.elapsed().as_secs_f64();
        match &event {
            Ok(ClientEvent::Frame(ldata)) => {
                println!("{elapsed:9.3}  {}", format_frame(ldata, &types))
            }
            Ok(ClientEvent::Other(cemi)) => println!("{elapsed:9.3}  {}", format_other(cemi)),
            Ok(ClientEvent::Connected { address }) => {
                eprintln!("{elapsed:9.3}  reconnected as {address}")
            }
            Ok(ClientEvent::Disconnected { reason }) => {
                eprintln!("{elapsed:9.3}  disconnected: {reason}; reconnecting")
            }
            Ok(ClientEvent::Closed) | Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                return Err(Failure::Runtime("connection closed".into()));
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                eprintln!("{elapsed:9.3}  ({n} frames dropped: output too slow)");
                continue;
            }
        }
        if matches!(event, Ok(ClientEvent::Frame(_) | ClientEvent::Other(_))) {
            seen += 1;
            if count.is_some_and(|c| seen >= c) {
                break;
            }
        }
    }
    client.close().await;
    Ok(())
}

fn format_frame(ldata: &LData, types: &Types) -> String {
    let ga = ldata.group_address();
    let dpt = ga.and_then(|ga| types.dpt(ga));
    let what = match ldata.apdu() {
        Some(Apdu::GroupValueRead) => "read".to_string(),
        Some(Apdu::GroupValueResponse(p)) => format!("response {}", format_value(p, dpt)),
        Some(Apdu::GroupValueWrite(p)) => format!("write {}", format_value(p, dpt)),
        Some(Apdu::Other { apci, data }) => format!(
            "apci {apci:#05x} {}",
            format_payload(&Payload::Bytes(data.clone()))
        ),
        None => format!("{:?}", ldata.tpdu),
    };
    let name = ga
        .and_then(|ga| types.name(ga))
        .map(|n| format!("{n}  "))
        .unwrap_or_default();
    format!(
        "{:<10} -> {:<10} {name}{what}",
        ldata.source.to_string(),
        ldata.destination.to_string()
    )
}

/// Datapoint types per group address: `--dpt` mappings first, then the `--project` export.
/// For `read` and `write`, a bare `--dpt DPT` applies to the one address given.
#[derive(Default)]
struct Types {
    single: Option<DptId>,
    overrides: HashMap<GroupAddress, DptId>,
    project: Option<GroupAddressTable>,
}

impl Types {
    fn load(args: &Args) -> Result<Self, Failure> {
        let mut types = Self::default();
        for mapping in args.flags("dpt") {
            match mapping.split_once('=') {
                Some((ga, dpt)) => {
                    types.overrides.insert(parse_ga(ga)?, parse_dpt(dpt)?);
                }
                None => types.single = Some(parse_dpt(mapping)?),
            }
        }
        if let Some(path) = args.flag("project") {
            let bytes = std::fs::read(path)
                .map_err(|e| Failure::Runtime(format!("cannot read {path}: {e}")))?;
            let table = GroupAddressTable::parse_bytes(&bytes)
                .map_err(|e| Failure::Runtime(format!("cannot import {path}: {e}")))?;
            types.project = Some(table);
        }
        Ok(types)
    }

    fn dpt(&self, ga: GroupAddress) -> Option<DptId> {
        self.single
            .or_else(|| self.overrides.get(&ga).copied())
            .or_else(|| self.project.as_ref().and_then(|p| p.dpt(ga)))
            .filter(|d| d.is_supported())
    }

    fn name(&self, ga: GroupAddress) -> Option<&str> {
        self.project
            .as_ref()?
            .get(ga)
            .map(|i| i.name.as_str())
            .filter(|n| !n.is_empty())
    }
}

fn format_other(cemi: &Cemi) -> String {
    match cemi {
        Cemi::Other { message_code, data } => {
            format!(
                "cEMI {message_code:#04x}: {}",
                format_payload(&Payload::Bytes(data.clone()))
            )
        }
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_values() {
        assert!(matches!(parse_value("1", None), Ok(Payload::Small(1))));
        assert!(matches!(parse_value("0x0c1a", None), Ok(Payload::Bytes(b)) if b == [0x0c, 0x1a]));
        assert!(parse_value("64", None).is_err());
        assert!(parse_value("0x0c1", None).is_err());
        assert!(parse_value("0xzz", None).is_err());
        let dpt = Some(DptId::new(9, 1));
        assert!(matches!(parse_value("21.5", dpt), Ok(Payload::Bytes(b)) if b == [0x0c, 0x33]));
        assert!(parse_value("hot", dpt).is_err());
    }

    #[test]
    fn value_formatting() {
        let p = Payload::Bytes(vec![0x0c, 0x33]);
        assert_eq!(format_value(&p, Some(DptId::new(9, 1))), "21.5 °C  (0c 33)");
        assert_eq!(format_value(&p, None), "0c 33");
        assert!(format_value(&Payload::Small(1), Some(DptId::new(9, 1))).contains("not a valid"));
    }

    #[test]
    fn endpoints() {
        assert!(matches!(parse_endpoint("10.0.0.5"), Ok(a) if a.port() == 3671));
        assert!(matches!(parse_endpoint("10.0.0.5:4000"), Ok(a) if a.port() == 4000));
        assert!(parse_endpoint("gateway.local").is_err());
    }
}
