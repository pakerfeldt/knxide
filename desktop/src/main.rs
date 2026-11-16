//! `knxide-desktop`: the knxide programmer as a desktop application.
//!
//! The same server as `knxide-programmer` runs inside this program, on a port of this machine
//! that the system picks, and a window shows its page. Nothing of the page is changed for it.
//! What a window needs and a browser tab does not is added here: a place to save an exported
//! project, a question before closing in the middle of an operation, and one running copy.
//!
//! ```sh
//! knxide-desktop              # the bus over KNXnet/IP routing, wired before wireless
//! knxide-desktop --simulate   # no bus: simulated devices
//! ```

// No console window beside the application on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{Ipv4Addr, SocketAddr};

use knxide_programmer::startup::{self, Options, Routing};
use knxide_programmer::{Workspace, serve};
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

/// The name in the title bar and on the dialogs.
const NAME: &str = "knxide";
/// The label Tauri knows the one window by.
const WINDOW: &str = "main";

const USAGE: &str = "\
knxide-desktop: program KNX devices from ETS project exports, in a window

USAGE:
    knxide-desktop [PROJECT] [--projects DIR] [--gateway HOST[:PORT] [--tcp]
                   | --routing [IP] [--address A.L.D] | --simulate] [--state FILE]
                   [--language LANG]

    The options are those of knxide-programmer, with one difference: without --gateway,
    --routing or --simulate the bus is reached by KNXnet/IP routing, on the interface chosen
    each time the bus is used (a wired one that is up before a wireless one).

    PROJECT              An ETS project export to list where it is and open at start.
    --projects DIR       Where the list of projects is kept.
    --gateway HOST[:PORT]  Tunnel through this KNX IP interface or router.
    --tcp                  Tunnel over TCP.
    --routing [IP]         KNXnet/IP routing, on the interface with this address.
    --address A.L.D        Routing: the individual address this tool sends from. Default 0.0.2.
    --simulate             No bus at all: devices are simulated from the project.
    --state FILE         Where the edits of PROJECT are kept.
    --language LANG      Language of the products' texts. Default en-US.
";

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "knxide_desktop=info,knxide_programmer=info,knxide_tokio=info".into()
            }),
        )
        .init();
    let options = match startup::parse(std::env::args().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            print!("{USAGE}");
            return;
        }
        Err(err) => {
            eprintln!("knxide-desktop: {err}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    let result = tauri::Builder::default()
        // Two copies would write the same list of projects and send from the same address:
        // starting it again brings the window that is open to the front.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window(WINDOW) {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let handle = app.handle().clone();
            let started = tauri::async_runtime::block_on(start(&options))
                .and_then(|(workspace, address)| open_window(&handle, workspace, address));
            if let Err(err) = started {
                // No window to show it in: say it in a dialog, and go when it is closed.
                tracing::error!("{err}");
                let app = handle.clone();
                handle
                    .dialog()
                    .message(err)
                    .title(NAME)
                    .kind(MessageDialogKind::Error)
                    .show(move |_| app.exit(1));
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let busy = window
                    .try_state::<Workspace>()
                    .and_then(|workspace| workspace.current())
                    .is_some_and(|programmer| programmer.running());
                if busy {
                    // Closing ends the program, and with it what it is doing on the bus.
                    api.prevent_close();
                    let window = window.clone();
                    window
                        .dialog()
                        .message(
                            "An operation is still running. Closing now stops it partway: a \
                             device that is being programmed is left without its application \
                             until it is programmed again.",
                        )
                        .title(NAME)
                        .kind(MessageDialogKind::Warning)
                        .parent(&window)
                        .buttons(MessageDialogButtons::OkCancelCustom(
                            "Close anyway".into(),
                            "Keep open".into(),
                        ))
                        .show(move |close| {
                            if close {
                                let _ = window.destroy();
                            }
                        });
                }
            }
        })
        .run(tauri::generate_context!());
    if let Err(err) = result {
        eprintln!("knxide-desktop: {err}");
        std::process::exit(1);
    }
}

/// Make the list of projects and serve the page on a port of this machine. Returns the list
/// and where the page is.
async fn start(options: &Options) -> Result<(Workspace, SocketAddr), String> {
    let bus = if options.names_bus() {
        options.bus().await?
    } else {
        // An application started from a launcher has no command line to name the bus on.
        // Routing needs nothing named, the interface is chosen when the bus is used, and
        // nothing is sent until an operation is asked for.
        let routing = Options {
            routing: Some(Routing::Auto),
            ..options.clone()
        };
        routing.bus().await?
    };
    let workspace = options.workspace(bus).await?;
    let listen = options
        .listen
        .unwrap_or_else(|| SocketAddr::from((Ipv4Addr::LOCALHOST, 0)));
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|error| format!("cannot listen on {listen}: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("cannot listen on {listen}: {error}"))?;
    let router = serve(workspace.clone());
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, router).await {
            tracing::error!("the page is no longer served: {error}");
        }
    });
    tracing::info!("the page is at http://{address}");
    Ok((workspace, address))
}

/// Open the window on the page at `address`.
fn open_window(app: &AppHandle, workspace: Workspace, address: SocketAddr) -> Result<(), String> {
    // A listener on every interface is reached from here through this machine itself.
    let host = if address.ip().is_unspecified() {
        Ipv4Addr::LOCALHOST.into()
    } else {
        address.ip()
    };
    let page = SocketAddr::new(host, address.port());
    let url: Url = format!("http://{page}/")
        .parse()
        .map_err(|error| format!("http://{page}/: {error}"))?;
    app.manage(workspace.clone());
    let handle = app.clone();
    WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::External(url.clone()))
        .title(NAME)
        .inner_size(1360.0, 880.0)
        .min_inner_size(900.0, 560.0)
        .on_navigation(move |to| {
            if to.origin() != url.origin() {
                // The page has no links that leave it; nothing else is shown in its place.
                return false;
            }
            match exported_project(to) {
                Some(id) => {
                    // What a browser would download is saved where the user says.
                    save_export(&handle, &workspace, id);
                    false
                }
                None => true,
            }
        })
        .build()
        .map_err(|error| format!("the window cannot be opened: {error}"))?;
    Ok(())
}

/// The project a page address asks to export: the `ID` of `/api/projects/ID/export`.
fn exported_project(url: &Url) -> Option<String> {
    let mut segments = url.path_segments()?;
    let found = (segments.next()? == "api" && segments.next()? == "projects")
        .then(|| segments.next())
        .flatten()?;
    (segments.next()? == "export" && segments.next().is_none() && !found.is_empty())
        .then(|| found.to_string())
}

/// Write the project `id` out as a project file and ask where to save it.
fn save_export(app: &AppHandle, workspace: &Workspace, id: String) {
    let app = app.clone();
    let workspace = workspace.clone();
    tauri::async_runtime::spawn(async move {
        let (name, bytes) = match workspace.export_listed(&id).await {
            Ok(exported) => exported,
            Err(err) => return failed(&app, format!("The project could not be exported: {err}")),
        };
        let mut dialog = app
            .dialog()
            .file()
            .set_title("Export project")
            .set_file_name(name)
            .add_filter("ETS project", &["knxproj"]);
        if let Some(window) = app.get_webview_window(WINDOW) {
            dialog = dialog.set_parent(&window);
        }
        let app = app.clone();
        dialog.save_file(move |chosen| {
            let Some(path) = chosen.and_then(|chosen| chosen.into_path().ok()) else {
                return;
            };
            if let Err(error) = std::fs::write(&path, bytes) {
                failed(
                    &app,
                    format!("{} could not be written: {error}", path.display()),
                );
            }
        });
    });
}

/// Tell the user that something they asked for did not happen.
fn failed(app: &AppHandle, message: String) {
    tracing::error!("{message}");
    let mut dialog = app
        .dialog()
        .message(message)
        .title(NAME)
        .kind(MessageDialogKind::Error);
    if let Some(window) = app.get_webview_window(WINDOW) {
        dialog = dialog.parent(&window);
    }
    dialog.show(|_| {});
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(path: &str) -> Option<String> {
        exported_project(&format!("http://127.0.0.1:4000{path}").parse().unwrap())
    }

    #[test]
    fn only_an_export_address_names_a_project() {
        assert_eq!(asked("/api/projects/p1a2b/export"), Some("p1a2b".into()));
        assert_eq!(asked("/"), None);
        assert_eq!(asked("/api/projects"), None);
        assert_eq!(asked("/api/projects/p1a2b"), None);
        assert_eq!(asked("/api/projects//export"), None);
        assert_eq!(asked("/api/projects/p1a2b/export/more"), None);
        assert_eq!(asked("/api/other/p1a2b/export"), None);
    }
}
