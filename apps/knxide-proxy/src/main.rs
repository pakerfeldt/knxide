//! `knxide-proxy`: share one tunnel to a KNX IP interface between any number of clients.
//!
//! ```sh
//! knxide-proxy --gateway 192.168.1.50
//! ```
//!
//! Clients (Home Assistant, ETS group monitor, knxide-monitor, …) then connect to this
//! machine on port 3671 as if it were the interface.

use std::net::{IpAddr, SocketAddr};

use knxide_core::IndividualAddress;
use knxide_ip::tunnel::Transport;
use knxide_proxy::{Proxy, ProxyConfig};

const USAGE: &str = "\
knxide-proxy: share one KNX IP tunnel between any number of clients

USAGE:
    knxide-proxy --gateway IP[:PORT] [OPTIONS]

OPTIONS:
    --gateway IP[:PORT]   The real KNX IP interface. Default: $KNXIDE_GATEWAY.
    --tcp                 Tunnel to it over TCP.
    --listen IP:PORT      Where clients connect. Default 0.0.0.0:3671.
    --advertise IP        Address to give clients, if this machine has several.
    --name NAME           Name shown in discovery. Default \"knxide proxy\".
    --addresses A-B       Individual addresses for clients, e.g. 15.15.241-15.15.254 (the
                          default). One per client; their count is the client limit.
    --no-discovery        Do not answer multicast search requests.

Clients get their own individual address; on the bus, all their frames carry the proxy's
one tunnel address. Group communication works fully; programming devices with ETS through
the proxy does not.

Other machines need UDP port 3671 open in this machine's firewall to reach the proxy.
";

fn parse_endpoint(text: &str) -> Result<SocketAddr, String> {
    text.parse::<SocketAddr>()
        .or_else(|_| {
            text.parse::<IpAddr>()
                .map(|ip| SocketAddr::new(ip, knxide_ip::frame::KNXNETIP_PORT))
        })
        .map_err(|_| format!("invalid address {text:?}"))
}

fn parse_range(text: &str) -> Result<Vec<IndividualAddress>, String> {
    let bad = || format!("invalid --addresses {text:?}, expected e.g. 15.15.241-15.15.254");
    let (first, last) = text.split_once('-').ok_or_else(bad)?;
    let (first, last): (IndividualAddress, IndividualAddress) = (
        first.trim().parse().map_err(|_| bad())?,
        last.trim().parse().map_err(|_| bad())?,
    );
    if first.raw() > last.raw() {
        return Err(bad());
    }
    Ok((first.raw()..=last.raw())
        .map(IndividualAddress::from_raw)
        .collect())
}

fn parse_args() -> Result<ProxyConfig, String> {
    let mut gateway = std::env::var("KNXIDE_GATEWAY")
        .ok()
        .filter(|g| !g.is_empty());
    let mut config = ProxyConfig::new(SocketAddr::from(([0, 0, 0, 0], 0)));
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--gateway" => gateway = Some(value("--gateway")?),
            "--tcp" => config.upstream.tunnel.transport = Transport::Tcp,
            "--listen" => {
                let v = value("--listen")?;
                config.listen = v.parse().map_err(|_| format!("invalid --listen {v:?}"))?;
            }
            "--advertise" => {
                let v = value("--advertise")?;
                config.advertise = Some(
                    v.parse()
                        .map_err(|_| format!("invalid --advertise {v:?}"))?,
                );
            }
            "--name" => config.server.name = value("--name")?,
            "--addresses" => config.server.client_addresses = parse_range(&value("--addresses")?)?,
            "--no-discovery" => config.discovery = false,
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    config.gateway = parse_endpoint(&gateway.ok_or("--gateway is required")?)?;
    Ok(config)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "knxide_proxy=info,knxide_tokio=info".into()),
        )
        .init();
    let config = match parse_args() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("knxide-proxy: {err}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let clients = config.server.client_addresses.len();
    let proxy = match Proxy::start(config).await {
        Ok(proxy) => proxy,
        Err(err) => {
            eprintln!("knxide-proxy: cannot start: {err}");
            std::process::exit(1);
        }
    };
    eprintln!(
        "knxide-proxy: clients connect to {} (up to {clients})",
        proxy.endpoint()
    );
    proxy
        .run_until(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_ranges() {
        let pool = parse_range("1.1.240-1.1.242").unwrap();
        assert_eq!(
            pool.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
            ["1.1.240", "1.1.241", "1.1.242"]
        );
        assert!(parse_range("1.1.242-1.1.240").is_err());
        assert!(parse_range("1.1.240").is_err());
    }
}
