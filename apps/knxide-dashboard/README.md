# knxide-dashboard

A customisable KNX dashboard for phone, tablet and desktop.

```sh
cargo install --path apps/knxide-dashboard --root ~/.local
knxide-dashboard --gateway 127.0.0.1 --project knx.xml   # 127.0.0.1 = knxide-proxy on this machine
# open http://<this machine>:8090 on any device on the home network
```

## Tiles

| Tile | Does |
|---|---|
| Switch | Tap to toggle; follows an optional status address |
| Dimmer | Tap to toggle, drag sideways to set the brightness (5.001); the tile fills to the level |
| Value | Shows a reading with its unit; doors left open and alarms are highlighted; the age shows once a value is over 30 minutes old |
| Temperature | Measured temperature, target with − / + (set the step), and the heating output (5.001) as a bar |
| Scene | Activates a scene (17.001 / 18.001) |
| Blinds | Up, stop, down; drag sideways to set the position |
| Heating mode | Auto, comfort, standby, eco, protection (20.102) |
| Button | Sends a fixed value to any address |

Pages hold rooms, and rooms hold tiles. Each room gets a heading that counts the lights that
are on, with **All off** once two or more are. Inside a room a tile drops the room from its
label, so "Hall tak" under "Hall" reads "Tak".

The pencil opens edit mode: add, edit, remove and reorder tiles (drag on desktop, where
dropping on a tile in another room moves it there; move buttons in a tile's editor on
touch), set a tile's room, add, rename and remove pages, set the title. **Suggest from
project…** reads a group range of your ETS export and proposes tiles, pairing commands with
their status addresses by name: "Kök tak bryt" + "Kök tak status" become one switch,
"… dim absolut" + "… status värde" one dimmer, and "Hall" + "Hall bör" + "Hall
faktiskt bör" + "Hall värmestatus" one temperature tile. Tiles whose labels start with
the same word ("Hall", "Rum 2") share a room. Untick what you do not want.

The page speaks English or Swedish (edit mode → Language): labels, on/off and open/closed
values, relative times, and decimal commas.

The layout is saved in `dashboard.json` (`--dashboard FILE`), which can also be edited by
hand. Every open page updates when it changes.

## Values

On connect the server reads every address a tile shows, so tiles have values at once, and
keeps them current from bus traffic. DPTs come from the project, with knxide-monitor's
`<project>.overrides.json` applied.

## Access

The server listens on `0.0.0.0:8090` so phones can reach it. There is no login: anyone on
your network who can open the page can switch things. Other devices need the port open in
this machine's firewall (for ufw: `sudo ufw allow 8090/tcp`). The server refuses WebSocket
connections from other websites' pages, so browsing elsewhere cannot drive your bus.

On a phone, "Add to Home Screen" makes it open full-screen like an app.

## Trying it without the bus

```sh
cargo run -p knxide-testkit --example demo_gateway -- --listen 127.0.0.1:13671 --project knx.xml
cargo run -p knxide-dashboard -- --gateway 127.0.0.1:13671 --listen 127.0.0.1:8091 --dashboard /tmp/demo-dashboard.json
```

The demo gateway simulates actuators: writes to command addresses come back on their status
addresses, found by name.
