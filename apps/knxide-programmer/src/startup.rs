//! What a program that serves the page is started with: the options of the command line, and
//! the bus and the list of projects made from them.
//!
//! Shared by the `knxide-programmer` binary, which serves the page to a browser, and by the
//! desktop program, which shows it in a window of its own.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use knxide_core::IndividualAddress;

use crate::interfaces::{self, Kind};
use crate::link::{BusAccess, WIRELESS_WARNING};
use crate::{BusConfig, Settings, Workspace};

/// The tool's individual address over routing unless `--address` says otherwise. The tool that
/// made the recordings sits at 0.0.1.
const DEFAULT_ADDRESS: u16 = 0x0002;

/// Which interface routing uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routing {
    /// The one chosen each time the bus is used: wired before wireless.
    Auto,
    /// The one with this address.
    On(Ipv4Addr),
}

/// The options of the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// A project to list where it is and open at start.
    pub project: Option<PathBuf>,
    /// Where the list of projects is kept; [`default_library`] when not given.
    pub projects: Option<PathBuf>,
    /// Where to serve the page; the program's own default when not given.
    pub listen: Option<SocketAddr>,
    /// Tunnel through this interface or router, `HOST[:PORT]`.
    pub gateway: Option<String>,
    /// Tunnel over TCP.
    pub tcp: bool,
    /// KNXnet/IP routing, and on which interface.
    pub routing: Option<Routing>,
    /// The individual address the tool sends from over routing.
    pub address: IndividualAddress,
    /// No bus: devices simulated from the project.
    pub simulate: bool,
    /// Where the edits of `project` are kept, when not beside it.
    pub state: Option<PathBuf>,
    /// The language of the products' texts.
    pub language: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            project: None,
            projects: None,
            listen: None,
            gateway: None,
            tcp: false,
            routing: None,
            address: IndividualAddress::from_raw(DEFAULT_ADDRESS),
            simulate: false,
            state: None,
            language: "en-US".into(),
        }
    }
}

/// Read the options from the arguments after the program's name. `Ok(None)` when help was
/// asked for; `Err` says in words what is wrong.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Options>, String> {
    let mut options = Options::default();
    let mut project = None;
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--listen" => {
                let v = value("--listen")?;
                options.listen = Some(v.parse().map_err(|_| format!("invalid --listen {v:?}"))?);
            }
            "--gateway" => options.gateway = Some(value("--gateway")?),
            "--tcp" => options.tcp = true,
            "--routing" => {
                // An address may follow, or "auto", or nothing of the kind.
                let next = args.peek().cloned().unwrap_or_default();
                options.routing = Some(if let Ok(address) = next.parse() {
                    args.next();
                    Routing::On(address)
                } else {
                    if next == "auto" {
                        args.next();
                    }
                    Routing::Auto
                });
            }
            "--address" => {
                let v = value("--address")?;
                options.address = v
                    .parse()
                    .map_err(|_| format!("{v:?} is not an individual address"))?;
            }
            "--simulate" => options.simulate = true,
            "--state" => options.state = Some(value("--state")?.into()),
            "--projects" => options.projects = Some(value("--projects")?.into()),
            "--language" => options.language = value("--language")?,
            "--project" => project = Some(value("--project")?),
            "-h" | "--help" => return Ok(None),
            other if other.starts_with('-') => return Err(format!("unknown option {other:?}")),
            other if project.is_none() => project = Some(other.to_string()),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    options.project = project.map(PathBuf::from);
    if options.state.is_some() && options.project.is_none() {
        return Err("--state goes with a PROJECT".into());
    }
    let chosen = [
        options.gateway.is_some(),
        options.routing.is_some(),
        options.simulate,
    ];
    if chosen.iter().filter(|chosen| **chosen).count() > 1 {
        return Err("choose one of --gateway, --routing and --simulate".into());
    }
    Ok(Some(options))
}

/// Where the list of projects is kept unless `--projects` says otherwise: `knxide/projects`
/// in the user's data directory (`$XDG_DATA_HOME`, else `~/.local/share`), or, where neither
/// is known, `knxide-projects` in the working directory.
pub fn default_library() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local").join("share"))
        });
    match data {
        Some(data) => data.join("knxide").join("projects"),
        None => PathBuf::from("knxide-projects"),
    }
}

/// Resolve `HOST[:PORT]` to the gateway's control endpoint.
async fn resolve(gateway: &str) -> Result<SocketAddr, String> {
    let target = if gateway.contains(':') {
        gateway.to_string()
    } else {
        format!("{gateway}:{}", knxide_ip::frame::KNXNETIP_PORT)
    };
    tokio::net::lookup_host(&target)
        .await
        .map_err(|error| format!("cannot resolve {gateway:?}: {error}"))?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| format!("{gateway:?} has no IPv4 address"))
}

impl Options {
    /// Whether a way to the bus was chosen (`--gateway`, `--routing` or `--simulate`).
    pub fn names_bus(&self) -> bool {
        self.gateway.is_some() || self.routing.is_some() || self.simulate
    }

    /// The directory the list of projects is kept in.
    pub fn library(&self) -> PathBuf {
        self.projects.clone().unwrap_or_else(default_library)
    }

    /// How the projects that are opened reach a bus. Routing on a named interface stays on
    /// it; without a name the interface is chosen each time the bus is used. Which one it is
    /// now, and what that means for downloads, is logged.
    pub async fn bus(&self) -> Result<BusConfig, String> {
        if self.simulate {
            // About the pace of a real bus, so that what is shown while programming can be
            // seen.
            return Ok(BusConfig::Simulated {
                tool: self.address,
                frame_delay: Duration::from_millis(12),
            });
        }
        if let Some(routing) = self.routing {
            return Ok(self.routing(routing));
        }
        if let Some(gateway) = &self.gateway {
            let gateway = resolve(gateway).await?;
            return Ok(BusConfig::Access(BusAccess::Tunnel {
                gateway,
                tcp: self.tcp,
            }));
        }
        Ok(BusConfig::None)
    }

    fn routing(&self, routing: Routing) -> BusConfig {
        let known = interfaces::list();
        let (interface, bus) = match routing {
            Routing::On(address) => {
                let interface = interfaces::with_address(&known, address);
                let access = BusAccess::Routing {
                    interface: interface.address,
                    name: interface.name.clone(),
                    kind: interface.kind,
                    address: self.address,
                    group: None,
                };
                (Some(interface), BusConfig::Access(access))
            }
            // Chosen again each time the bus is used; what is said here is how it is now.
            Routing::Auto => (
                interfaces::choose(&known, interfaces::system_default()),
                BusConfig::AutoRouting { tool: self.address },
            ),
        };
        let Some(interface) = interface else {
            tracing::warn!(
                "no network interface with an IPv4 address was found; one is looked for \
                 again when an operation starts"
            );
            return bus;
        };
        // Said in so many words: which interface, and what that means for downloads.
        tracing::info!(
            "routing on {interface} as {}, address {}",
            self.address,
            interface.address
        );
        match interface.kind {
            Kind::Wireless => {
                tracing::warn!("{WIRELESS_WARNING}");
                if let Some(wired) = known
                    .iter()
                    .find(|other| other.up && other.kind == Kind::Wired)
                {
                    tracing::warn!(
                        "a wired interface is up: {} at {}; start with --routing {} or \
                         plain --routing to use it",
                        wired.name,
                        wired.address,
                        wired.address
                    );
                }
            }
            Kind::Unknown => tracing::warn!(
                "whether {} is wired or wireless could not be told; multicast over Wi-Fi loses \
                 frames, so make sure downloads go over a cable",
                interface.address
            ),
            Kind::Wired => {}
        }
        bus
    }

    /// The list of projects, reaching the bus as `bus` says, with the project named on the
    /// command line listed and opened.
    pub async fn workspace(&self, bus: BusConfig) -> Result<Workspace, String> {
        let library = self.library();
        let workspace = Workspace::library(
            library.clone(),
            bus,
            Settings::default(),
            self.language.clone(),
        )?;
        tracing::info!(directory = %library.display(), "projects are kept here");
        if let Some(project) = &self.project {
            let id = workspace.register(project, self.state.clone())?;
            workspace.open_listed(&id).await?;
        }
        Ok(workspace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Option<Options>, String> {
        parse(args.iter().map(ToString::to_string))
    }

    #[test]
    fn nothing_given_is_the_defaults() {
        let options = parsed(&[]).unwrap().unwrap();
        assert_eq!(options, Options::default());
        assert!(!options.names_bus());
        assert_eq!(options.address.to_string(), "0.0.2");
    }

    #[test]
    fn routing_takes_an_address_the_word_auto_or_nothing() {
        let on = parsed(&["--routing", "192.168.1.20"]).unwrap().unwrap();
        assert_eq!(
            on.routing,
            Some(Routing::On(Ipv4Addr::new(192, 168, 1, 20)))
        );
        let auto = parsed(&["--routing", "auto", "house.knxproj"])
            .unwrap()
            .unwrap();
        assert_eq!(auto.routing, Some(Routing::Auto));
        assert_eq!(auto.project, Some(PathBuf::from("house.knxproj")));
        // What follows is not an address: it is the project.
        let bare = parsed(&["--routing", "house.knxproj"]).unwrap().unwrap();
        assert_eq!(bare.routing, Some(Routing::Auto));
        assert_eq!(bare.project, Some(PathBuf::from("house.knxproj")));
    }

    #[test]
    fn one_way_to_the_bus_at_most() {
        assert!(parsed(&["--simulate", "--routing"]).is_err());
        assert!(parsed(&["--gateway", "10.0.0.2", "--simulate"]).is_err());
        assert!(
            parsed(&["--gateway", "10.0.0.2", "--tcp"])
                .unwrap()
                .unwrap()
                .names_bus()
        );
    }

    #[test]
    fn help_is_not_options_and_mistakes_are_named() {
        assert_eq!(parsed(&["--help"]), Ok(None));
        assert!(parsed(&["--frobnicate"]).unwrap_err().contains("unknown"));
        assert!(
            parsed(&["--state", "x.json"])
                .unwrap_err()
                .contains("PROJECT")
        );
        assert!(parsed(&["a.knxproj", "b.knxproj"]).is_err());
        assert!(parsed(&["--listen"]).unwrap_err().contains("needs a value"));
    }
}
