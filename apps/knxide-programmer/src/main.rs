//! `knxide-programmer`: download KNX application programs from a web page.
//!
//! Reads an ETS project export, shows which of its devices it can program and what a download
//! would send, compares a device with that, and downloads.
//!
//! ```sh
//! knxide-programmer house.knxproj --gateway 192.168.1.50
//! ```

use std::net::SocketAddr;

use knxide_programmer::startup::{self, Options};
use knxide_programmer::{Workspace, serve};

const USAGE: &str = "\
knxide-programmer: download KNX application programs from a web page

USAGE:
    knxide-programmer [PROJECT] [--projects DIR] [--gateway HOST[:PORT] [--tcp]
                      | --routing [IP] [--address A.L.D] | --simulate] [--state FILE]
                      [--language LANG] [--listen IP:PORT]

    PROJECT              An ETS project export (.knxproj, without password), or a directory
                         with its unpacked content. It is listed where it is and opened at
                         start. It is only read, never changed. Without it the page starts
                         with the list of projects.
    --projects DIR       Where the list of projects is kept, with the project files brought
                         in through the page. Default: knxide/projects in the user's data
                         directory.
    --state FILE         Where the edits of PROJECT are kept. Default: beside the project, as
                         <project file name>.knxide.json.
    --language LANG      Language of the products' texts where they have it, for example
                         en-US or de-DE. Default en-US.

BUS (without one of these the page shows the inventory and the plans, nothing more):
    --gateway HOST[:PORT]  Tunnel through this KNX IP interface or router. Port 3671 unless
                           given. The tunnel is opened for each operation and closed after it.
    --tcp                  Tunnel over TCP.
    --routing [IP]         KNXnet/IP routing through any KNX IP router. Frames stay on the local
                           network (TTL 1). Without IP (or with the word auto) the interface is
                           chosen for you each time the bus is used: a wired one that is up
                           before a wireless one. With IP, the interface with that address is
                           used whatever it is.
                           Routing is multicast, and Wi-Fi loses multicast: do downloads over
                           a cable.
    --address A.L.D        Routing: the individual address this tool sends from. Default
                           0.0.2. It must not be in use by anything else, ETS included.
    --simulate             No bus at all: devices are simulated from the project, to try the
                           page without touching an installation.

SERVER:
    --listen IP:PORT       Where to serve the page. Default 127.0.0.1:8085.
                           There is no login: whoever can open the page can reprogram your
                           devices. Keep it local unless you trust the whole network.
";

/// Where the page is served unless `--listen` says otherwise.
const LISTEN: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8085);

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "knxide_programmer=info,knxide_tokio=info".into()),
        )
        .init();
    let options = match startup::parse(std::env::args().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            print!("{USAGE}");
            return;
        }
        Err(err) => {
            eprintln!("knxide-programmer: {err}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let workspace = match start(&options).await {
        Ok(workspace) => workspace,
        Err(err) => {
            eprintln!("knxide-programmer: {err}");
            std::process::exit(1);
        }
    };
    let listen = options.listen.unwrap_or(LISTEN);
    let listener = match tokio::net::TcpListener::bind(listen).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("knxide-programmer: cannot listen on {listen}: {err}");
            std::process::exit(1);
        }
    };
    if !listen.ip().is_loopback() {
        tracing::warn!("listening on {listen}: anyone who can reach it can reprogram devices");
    }
    eprintln!(
        "knxide-programmer on http://{}",
        listener.local_addr().unwrap_or(listen)
    );
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let server = axum::serve(listener, serve(workspace)).with_graceful_shutdown(shutdown);
    if let Err(err) = server.await {
        eprintln!("knxide-programmer: {err}");
    }
}

/// The bus and the list of projects the options ask for.
async fn start(options: &Options) -> Result<Workspace, String> {
    let bus = options.bus().await?;
    options.workspace(bus).await
}
