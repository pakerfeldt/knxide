//! `knxide-monitor`: a web console for a KNX bus.
//!
//! Serves one page and a WebSocket. The page shows bus traffic live, browses the group
//! addresses from an ETS export, and sends values to any address.
//!
//! ```sh
//! knxide-monitor --gateway 192.168.1.50 --project knx.xml
//! ```

use std::net::SocketAddr;

use knxide_monitor::{Bus, HealthConfig, Transport, app};
use knxide_project::GroupAddressTable;

const USAGE: &str = "\
knxide-monitor: a web console for a KNX bus

USAGE:
    knxide-monitor [--listen IP:PORT] [--gateway IP[:PORT]] [--tcp] [--project FILE] [--connect]

OPTIONS:
    --listen IP:PORT     Where to serve the page. Default 127.0.0.1:8080.
                         Anyone who can open the page can write to your bus: keep it local
                         unless the network is trusted.
    --gateway IP[:PORT]  Default gateway shown on the page. Default: $KNXIDE_GATEWAY.
    --tcp                Default to tunnelling over TCP.
    --project FILE       ETS group address export (CSV or XML). Default: ./knx.xml if present.
    --connect            Connect to the gateway at start-up.
    --stuck-after HOURS  Health: report a measured value as stuck when it repeats unchanged
                         this long. Default 6.
";

struct Config {
    listen: SocketAddr,
    gateway: Option<String>,
    transport: Transport,
    project: Option<String>,
    connect: bool,
    health: HealthConfig,
}

fn parse_args() -> Result<Config, String> {
    let mut config = Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
        gateway: std::env::var("KNXIDE_GATEWAY")
            .ok()
            .filter(|g| !g.is_empty()),
        transport: Transport::Udp,
        project: std::path::Path::new("knx.xml")
            .exists()
            .then(|| "knx.xml".to_string()),
        connect: false,
        health: HealthConfig::default(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--listen" => {
                let v = value("--listen")?;
                config.listen = v.parse().map_err(|_| format!("invalid --listen {v:?}"))?;
            }
            "--gateway" => config.gateway = Some(value("--gateway")?),
            "--project" => config.project = Some(value("--project")?),
            "--tcp" => config.transport = Transport::Tcp,
            "--connect" => config.connect = true,
            "--stuck-after" => {
                let v = value("--stuck-after")?;
                let hours: f64 = v
                    .parse()
                    .ok()
                    .filter(|h: &f64| *h > 0.0 && h.is_finite())
                    .ok_or_else(|| format!("invalid --stuck-after {v:?}"))?;
                config.health.stuck_after = std::time::Duration::from_secs_f64(hours * 3600.0);
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(config)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "knxide_monitor=info,knxide_tokio=info".into()),
        )
        .init();
    let config = match parse_args() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("knxide-monitor: {err}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    let table = match &config.project {
        Some(path) => match std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|b| GroupAddressTable::parse_bytes(&b).map_err(|e| e.to_string()))
        {
            Ok(table) => {
                tracing::info!(path, entries = table.len(), "project loaded");
                table
            }
            Err(err) => {
                eprintln!("knxide-monitor: cannot load {path}: {err}");
                std::process::exit(1);
            }
        },
        None => GroupAddressTable::default(),
    };
    let file = config.project.as_deref().map(|p| {
        std::path::Path::new(p)
            .file_name()
            .map_or_else(|| p.to_string(), |n| n.to_string_lossy().into_owned())
    });
    let overrides_file = config
        .project
        .as_deref()
        .map(|p| std::path::Path::new(p).with_extension("overrides.json"));
    let bus = Bus::with_health(
        config.gateway.clone(),
        config.transport,
        table,
        file,
        config.health,
    );

    if let Some(path) = overrides_file {
        match bus.use_overrides_file(path.clone()) {
            Ok(0) => {}
            Ok(n) => tracing::info!(path = %path.display(), n, "DPT overrides loaded"),
            Err(err) => {
                eprintln!("knxide-monitor: cannot load DPT overrides: {err}");
                std::process::exit(1);
            }
        }
    }

    if config.connect {
        match &config.gateway {
            Some(gateway) => {
                let (bus, gateway, transport) = (bus.clone(), gateway.clone(), config.transport);
                tokio::spawn(async move {
                    if let Err(err) = bus.connect(&gateway, transport).await {
                        tracing::warn!(%err, "could not connect at start-up");
                    }
                });
            }
            None => tracing::warn!("--connect without a gateway; ignoring"),
        }
    }

    let listener = match tokio::net::TcpListener::bind(config.listen).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("knxide-monitor: cannot listen on {}: {err}", config.listen);
            std::process::exit(1);
        }
    };
    if !config.listen.ip().is_loopback() {
        tracing::warn!(
            "listening on {}: anyone who can reach it can write to the bus",
            config.listen
        );
    }
    eprintln!(
        "knxide-monitor on http://{}",
        listener.local_addr().unwrap_or(config.listen)
    );
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let server = axum::serve(listener, app(bus.clone())).with_graceful_shutdown(shutdown);
    if let Err(err) = server.await {
        eprintln!("knxide-monitor: {err}");
    }
    bus.disconnect().await;
}
