//! `knxide-mcp`: a Model Context Protocol server for a KNX bus.
//!
//! By default an MCP client (Claude Code, Claude Desktop, …) starts it and talks JSON-RPC on
//! stdin and stdout. With `--listen` it runs as a long-lived Streamable HTTP server instead.
//! Logs go to stderr.
//!
//! ```sh
//! claude mcp add knx -- knxide-mcp --gateway 192.168.1.50 --project /path/to/knx.xml
//! ```

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use knxide_core::dpt::DptId;
use knxide_core::{GroupAddress, IndividualAddress};
use knxide_ip::tunnel::Transport;
use knxide_mcp::{Bus, Link, Server};
use knxide_project::{GroupAddressInfo, GroupAddressTable};
use knxide_tokio::RoutingOptions;

const USAGE: &str = "\
knxide-mcp: Model Context Protocol server for a KNX bus (stdio or Streamable HTTP)

USAGE:
    knxide-mcp [--gateway IP[:PORT] [--tcp] | --routing [--interface IP] [--address IA]]
               [--project FILE] [--allow-write] [--lazy] [--listen IP:PORT]

CONNECTION (tunnelling by default, or routing through a KNX IP router):
    --gateway IP[:PORT]  KNX IP interface or router to tunnel to. Default: $KNXIDE_GATEWAY.
    --tcp                Tunnel over TCP instead of UDP.
    --routing            Use KNXnet/IP routing (multicast 224.0.23.12) instead of a tunnel;
                         takes no tunnel slot. Needs a KNX IP router. Default: $KNXIDE_ROUTING
                         (1/true/yes). Overrides --gateway.
    --interface IP       Routing: local interface address for multicast. Default:
                         $KNXIDE_INTERFACE, else the interface the OS routes 224.0.23.12 via.
    --address IA         Routing: our individual address. Default: $KNXIDE_ADDRESS, else
                         15.15.250.

OPTIONS:
    --project FILE       ETS group address export (CSV or XML), for names and DPTs.
                         Default: $KNXIDE_PROJECT, else ./knx.xml if present. DPT overrides
                         are read from FILE's .overrides.json sibling, as knxide-monitor saves
                         them.
    --allow-write        Offer the write_group_address tool. Off by default: the model can
                         only look up and read.
    --lazy               Connect on the first tool call instead of at start-up.
    --listen IP:PORT     Serve MCP over HTTP at http://IP:PORT/mcp instead of stdio.
                         Default: $KNXIDE_MCP_LISTEN. Clients must send
                         `Authorization: Bearer $KNXIDE_MCP_TOKEN`; without a token set, only
                         a loopback address is allowed.

Add it to Claude Code with:
    claude mcp add knx -- knxide-mcp --gateway 192.168.1.50 --project /path/to/knx.xml
";

struct Config {
    gateway: Option<String>,
    transport: Transport,
    routing: bool,
    interface: Option<Ipv4Addr>,
    address: Option<IndividualAddress>,
    project: Option<String>,
    allow_write: bool,
    lazy: bool,
    listen: Option<SocketAddr>,
    token: Option<String>,
}

fn parse_args() -> Result<Config, String> {
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let mut config = Config {
        gateway: env("KNXIDE_GATEWAY"),
        transport: Transport::Udp,
        routing: env("KNXIDE_ROUTING")
            .map(|v| parse_bool("KNXIDE_ROUTING", &v))
            .transpose()?
            .unwrap_or(false),
        interface: None,
        address: None,
        project: env("KNXIDE_PROJECT")
            .or_else(|| Path::new("knx.xml").exists().then(|| "knx.xml".to_string())),
        allow_write: false,
        lazy: false,
        listen: None,
        token: env("KNXIDE_MCP_TOKEN").map(|t| t.trim().to_string()),
    };
    let parse_listen = |v: String| {
        v.parse::<SocketAddr>()
            .map_err(|_| format!("invalid listen address {v:?} (want IP:PORT)"))
    };
    config.listen = env("KNXIDE_MCP_LISTEN").map(parse_listen).transpose()?;
    let parse_interface = |v: String| {
        v.parse::<Ipv4Addr>()
            .map_err(|_| format!("invalid interface {v:?} (want an IPv4 address)"))
    };
    let parse_address = |v: String| {
        v.parse::<IndividualAddress>()
            .map_err(|_| format!("invalid individual address {v:?} (want e.g. 15.15.250)"))
    };
    config.interface = env("KNXIDE_INTERFACE").map(parse_interface).transpose()?;
    config.address = env("KNXIDE_ADDRESS").map(parse_address).transpose()?;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--gateway" => config.gateway = Some(value("--gateway")?),
            "--project" => config.project = Some(value("--project")?),
            "--tcp" => config.transport = Transport::Tcp,
            "--routing" => config.routing = true,
            "--interface" => config.interface = Some(parse_interface(value("--interface")?)?),
            "--address" => config.address = Some(parse_address(value("--address")?)?),
            "--allow-write" => config.allow_write = true,
            "--lazy" => config.lazy = true,
            "--listen" => config.listen = Some(parse_listen(value("--listen")?)?),
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    if let Some(listen) = config.listen
        && config.token.is_none()
        && !listen.ip().is_loopback()
    {
        return Err(format!(
            "--listen {listen} without KNXIDE_MCP_TOKEN would let anyone on the network use \
             the bus; set a token or listen on 127.0.0.1"
        ));
    }
    Ok(config)
}

fn parse_bool(name: &str, value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!("{name}={value:?}: want 1/true/yes or 0/false/no")),
    }
}

/// How to reach the bus, from the connection options.
fn link(config: &Config) -> Option<Link> {
    if config.routing {
        if let Some(gateway) = &config.gateway {
            tracing::info!(gateway, "routing: ignoring the gateway");
        }
        let mut options = RoutingOptions {
            interface: config.interface,
            ..RoutingOptions::default()
        };
        if let Some(address) = config.address {
            options.router.individual_address = address;
        }
        return Some(Link::Routing(options));
    }
    if config.interface.is_some() || config.address.is_some() {
        tracing::warn!("--interface and --address only apply with --routing; ignoring them");
    }
    config.gateway.clone().map(|gateway| Link::Tunnel {
        gateway,
        transport: config.transport,
    })
}

#[tokio::main]
async fn main() {
    // stdout carries the protocol: logs must go to stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "knxide_mcp=info,knxide_tokio=info".into()),
        )
        .init();
    let config = match parse_args() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("knxide-mcp: {err}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    let table = match &config.project {
        Some(path) => match load_project(Path::new(path)) {
            Ok(table) => {
                tracing::info!(path, entries = table.len(), "project loaded");
                table
            }
            Err(err) => {
                eprintln!("knxide-mcp: cannot load {path}: {err}");
                std::process::exit(1);
            }
        },
        None => {
            tracing::warn!("no ETS project: group addresses have no names or DPTs");
            GroupAddressTable::default()
        }
    };
    let file = config.project.as_deref().map(|p| {
        Path::new(p)
            .file_name()
            .map_or_else(|| p.to_string(), |n| n.to_string_lossy().into_owned())
    });
    let link = link(&config);
    let has_link = link.is_some();
    let bus = Bus::new(link, table, file);
    if !has_link {
        tracing::warn!("no bus: only find_group_addresses works; set --gateway or --routing");
    } else if !config.lazy {
        let bus = bus.clone();
        tokio::spawn(async move {
            if let Err(err) = bus.client().await {
                tracing::warn!(%err, "could not connect at start-up; will retry on use");
            }
        });
    }
    if config.allow_write {
        tracing::info!("writing to the bus is enabled");
    }

    let server = Arc::new(Server::new(bus.clone(), config.allow_write));
    match config.listen {
        None => {
            // Ends when the client closes stdin. No signal handler here: a pending stdin read
            // would hold up runtime shutdown, and the default action already exits.
            let stdin = tokio::io::BufReader::new(tokio::io::stdin());
            if let Err(err) = server.serve(stdin, tokio::io::stdout()).await {
                tracing::error!(%err, "stdio failed");
            }
        }
        Some(listen) => serve_http(server, listen, config.token).await,
    }
    // Free the tunnel on the interface; they only have a few.
    bus.close().await;
}

async fn serve_http(server: Arc<Server>, listen: SocketAddr, token: Option<String>) {
    let listener = match tokio::net::TcpListener::bind(listen).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("knxide-mcp: cannot listen on {listen}: {err}");
            std::process::exit(1);
        }
    };
    if token.is_none() {
        tracing::warn!("no KNXIDE_MCP_TOKEN: any local process can use the bus");
    }
    tracing::info!(
        "MCP over HTTP at http://{}{}",
        listener.local_addr().unwrap_or(listen),
        knxide_mcp::http::PATH
    );
    let app = knxide_mcp::http::router(server, token);
    if let Err(err) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        tracing::error!(%err, "HTTP server failed");
    }
}

/// Ctrl-C, or SIGTERM (`docker stop`; as PID 1 it is otherwise ignored).
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut term) => {
                term.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
    tracing::info!("shutting down");
}

/// The export at `path`, with DPT overrides from `<path>.overrides.json` applied.
fn load_project(path: &Path) -> Result<GroupAddressTable, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut table = GroupAddressTable::parse_bytes(&bytes).map_err(|e| e.to_string())?;
    let overrides_path = path.with_extension("overrides.json");
    let text = match std::fs::read_to_string(&overrides_path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(table),
        Err(err) => return Err(format!("{}: {err}", overrides_path.display())),
    };
    let overrides =
        parse_overrides(&text).map_err(|e| format!("{}: {e}", overrides_path.display()))?;
    tracing::info!(path = %overrides_path.display(), n = overrides.len(), "DPT overrides loaded");
    for (address, dpt) in overrides {
        let mut entry = table.get(address).cloned().unwrap_or(GroupAddressInfo {
            address,
            name: String::new(),
            description: String::new(),
            dpt: None,
            path: Vec::new(),
        });
        entry.dpt = Some(dpt);
        table.insert(entry);
    }
    Ok(table)
}

/// `{"5/0/1": "14"}`, as knxide-monitor writes it.
fn parse_overrides(text: &str) -> Result<BTreeMap<GroupAddress, DptId>, String> {
    let raw: BTreeMap<String, String> = serde_json::from_str(text).map_err(|e| e.to_string())?;
    raw.into_iter()
        .map(|(ga, dpt)| {
            let ga = ga
                .parse()
                .map_err(|_| format!("{ga:?} is not a group address"))?;
            let dpt = dpt.parse().map_err(|_| format!("{dpt:?} is not a DPT"))?;
            Ok((ga, dpt))
        })
        .collect()
}
