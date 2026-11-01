# knxide-proxy

Share one tunnel to a KNX IP interface between any number of clients.

KNX IP interfaces offer only a few tunnels (often 4–8). knxide-proxy holds one, and acts as a
KNXnet/IP gateway itself: Home Assistant, knxide-monitor, the knxide CLI, xknx scripts and
visualisations all connect to the proxy instead of the interface.

```sh
cargo install knxide-proxy
knxide-proxy --gateway 192.168.1.50
# clients now use this machine's IP, port 3671
```

## How it behaves

- Each client gets its own individual address from a pool (default `15.15.241`–`15.15.254`,
  so up to 14 clients; change with `--addresses`).
- A client's write goes to the bus; the client's confirmation waits for the real
  interface's. Once confirmed, the other clients see the write too, as with several tunnels
  on one interface. Bus traffic goes to every client.
- On the bus, all clients' frames carry the proxy's one tunnel address.
- If the interface goes away, the proxy keeps reconnecting. Meanwhile clients are told
  (their heartbeats are answered with "KNX connection error") and writes fail at once
  instead of timing out.
- Answers discovery (search and description), so clients can find it. `--no-discovery` turns
  multicast search off.

## Limits

- Group communication works fully. Programming devices with ETS through the proxy does not:
  replies from a device go to the proxy's bus address, not to one client.
- Clients connect over UDP. The proxy can reach the interface over UDP or TCP (`--tcp`).
- Other machines must be able to reach UDP port 3671 on the proxy's host; open it in that
  host's firewall (for ufw: `sudo ufw allow 3671/udp`).

## Running it permanently

As a systemd user service, `~/.config/systemd/user/knxide-proxy.service`:

```ini
[Unit]
Description=knxide KNX tunnelling proxy
After=network-online.target

[Service]
ExecStart=%h/.cargo/bin/knxide-proxy --gateway 192.168.1.50
Restart=on-failure

[Install]
WantedBy=default.target
```

then `systemctl --user enable --now knxide-proxy`.
