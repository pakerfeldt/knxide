# knxide-desktop

The knxide programmer as a desktop application. It is `apps/knxide-programmer` with a window
around it: the same server runs inside the program, on a port of this machine that the system
picks, and the window shows its page. The page and the API are not changed for it; everything
about what the tool does is in [the programmer's README](../apps/knxide-programmer/README.md).

Built with [Tauri 2](https://tauri.app), which shows the page in the system's own web view
(WebKitGTK on Linux, WebView2 on Windows, WKWebView on macOS). **Only Linux has been run**
(Hyprland on Wayland); nothing in the code is specific to it, but the other systems are
untried.

This crate is outside the main workspace, like `fuzz/`: it brings GTK and WebKitGTK with it,
and the library crates, their checks and their dependency policy stay free of that. It has
its own `Cargo.lock` and `target/`.

## Install (Linux)

```sh
desktop/install.sh            # build, then: ~/.local/bin/knxide, a launcher entry, the icon
desktop/install.sh --remove   # take those away again
```

Needs at build time and at run time: `webkit2gtk-4.1` and `gtk3`. At run time also a desktop
portal for the file chooser (`xdg-desktop-portal` with a backend such as
`xdg-desktop-portal-gtk`) and `zenity` for messages.

Or without installing: `cargo run --release --manifest-path desktop/Cargo.toml`.

## What differs from the page in a browser

- **The bus.** Started without options, as a launcher starts it, the bus is reached by
  KNXnet/IP routing from `0.0.2`, on the interface chosen each time the bus is used: a wired
  one that is up before a wireless one. Nothing is sent until an operation is asked for. The
  options of `knxide-programmer` (`--gateway`, `--routing IP`, `--address`, `--simulate`,
  `--projects`, `--language`, a `PROJECT`) are taken on the command line as there; `knxide
  --help` lists them. There is no settings screen yet: to make another way to the bus the
  standing one, put the option on the `Exec=` line of
  `~/.local/share/applications/knxide.desktop`.
- **Export** asks where to save the project file, in the system's file chooser, where a
  browser would download it.
- **Closing the window ends the program**, and with it whatever it is doing on the bus. While
  an operation runs, closing asks first: a download that is cut off leaves the device without
  its application until it is programmed again.
- **One copy.** Starting it again brings the open window to the front. Do not run
  `knxide-programmer` on the same list of projects at the same time: both would write the
  same files and send from the same individual address.
- The page is still served on `127.0.0.1`, on a port that changes with every start, to
  anything on this machine that asks. As with the server there is no login.

## Checks

```sh
cd desktop
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

What was tried by hand, with simulated devices: the page in the window, export through the
file chooser (the saved file is a readable project archive), the file chooser of *New
project*, the question when closing during a download and both of its answers, the message
when the list of projects cannot be made, and a second start. Tauri's own bundles (`cargo
tauri build`: deb, rpm, AppImage, msi, dmg) are configured in `tauri.conf.json` but have not
been built.
