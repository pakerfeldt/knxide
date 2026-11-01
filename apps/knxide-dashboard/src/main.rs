//! `knxide-dashboard`: a customisable KNX dashboard for phone, tablet and desktop.
//!
//! ```sh
//! knxide-dashboard --gateway 127.0.0.1 --project knx.xml
//! ```

use std::net::SocketAddr;
use std::path::PathBuf;

use knxide_dashboard::app::{App, apply_overrides, parse_endpoint};
use knxide_dashboard::config::Dashboard;
use knxide_dashboard::router;
use knxide_ip::tunnel::Transport;
use knxide_project::GroupAddressTable;
use knxide_tokio::TunnelOptions;

const USAGE: &str = "\
knxide-dashboard: a customisable KNX dashboard for phone, tablet and desktop

USAGE:
    knxide-dashboard [--gateway IP[:PORT]] [OPTIONS]

OPTIONS:
    --gateway IP[:PORT]  Gateway or knxide-proxy. Default: $KNXIDE_GATEWAY. Running
                         knxide-proxy and pointing this at it saves a tunnel.
    --tcp                Tunnel over TCP.
    --listen IP:PORT     Where to serve the page. Default 0.0.0.0:8090 (the home network).
    --project FILE       ETS group address export. Default ./knx.xml if present.
                         <project>.overrides.json from knxide-monitor is applied too.
    --dashboard FILE     Layout, edited in the page. Default ./dashboard.json.

Anyone who can open the page can use it: other devices need TCP port 8090 open in this
machine's firewall, and anyone on the network can then switch things.
";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "knxide_dashboard=info,knxide_tokio=info".into()),
        )
        .init();
    let fail = |msg: String| -> ! {
        eprintln!("knxide-dashboard: {msg}\n\n{USAGE}");
        std::process::exit(2);
    };

    let mut gateway = std::env::var("KNXIDE_GATEWAY")
        .ok()
        .filter(|g| !g.is_empty());
    let mut listen: SocketAddr = SocketAddr::from(([0, 0, 0, 0], 8090));
    let mut project = std::path::Path::new("knx.xml")
        .exists()
        .then(|| PathBuf::from("knx.xml"));
    let mut dashboard_file = PathBuf::from("dashboard.json");
    // Our own writes come back as frames, so tiles update at once.
    let mut options = TunnelOptions {
        local_echo: true,
        ..TunnelOptions::default()
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .unwrap_or_else(|| fail(format!("{name} needs a value")))
        };
        match arg.as_str() {
            "--gateway" => gateway = Some(value("--gateway")),
            "--tcp" => options.tunnel.transport = Transport::Tcp,
            "--listen" => {
                let v = value("--listen");
                listen = v
                    .parse()
                    .unwrap_or_else(|_| fail(format!("invalid --listen {v:?}")));
            }
            "--project" => project = Some(PathBuf::from(value("--project"))),
            "--dashboard" => dashboard_file = PathBuf::from(value("--dashboard")),
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            other => fail(format!("unknown option {other:?}")),
        }
    }
    let gateway_text = gateway.unwrap_or_else(|| fail("--gateway is required".into()));
    let gateway = parse_endpoint(&gateway_text).unwrap_or_else(|e| fail(e));

    let table = match &project {
        Some(path) => {
            let bytes = std::fs::read(path)
                .unwrap_or_else(|e| fail(format!("cannot read {}: {e}", path.display())));
            let table = GroupAddressTable::parse_bytes(&bytes)
                .unwrap_or_else(|e| fail(format!("{}: {e}", path.display())));
            apply_overrides(table, &path.with_extension("overrides.json"))
        }
        None => GroupAddressTable::default(),
    };
    let dashboard = Dashboard::load(&dashboard_file).unwrap_or_else(|e| fail(e));
    let app = App::new(gateway_text, table, dashboard, Some(dashboard_file));
    tokio::spawn(app.clone().run_bus(gateway, options));

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .unwrap_or_else(|e| fail(format!("cannot listen on {listen}: {e}")));
    eprintln!("knxide-dashboard on http://{listen}");
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    if let Err(err) = axum::serve(listener, router(app))
        .with_graceful_shutdown(shutdown)
        .await
    {
        eprintln!("knxide-dashboard: {err}");
    }
}
