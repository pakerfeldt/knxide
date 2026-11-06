//! The server end to end: JSON-RPC in, against the fake gateway.

use std::sync::Arc;
use std::time::Duration;

use knxide_core::{Apdu, Cemi, LData, Payload, ga};
use knxide_ip::Frame;
use knxide_ip::tunnel::Transport;
use knxide_mcp::{Bus, Link, Server};
use knxide_project::GroupAddressTable;
use knxide_testkit::FakeGateway;
use knxide_tokio::RoutingOptions;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const PROJECT: &str = r#"<GroupAddress-Export xmlns="http://knx.org/xml/ga-export/01">
  <GroupRange Name="Climate" RangeStart="2048" RangeEnd="4095">
    <GroupRange Name="Living" RangeStart="2048" RangeEnd="2303">
      <GroupAddress Name="Living temperature" Address="1/2/4" DPTs="DPST-9-1" />
      <GroupAddress Name="Living light" Address="1/2/3" DPTs="DPST-1-1" />
      <GroupAddress Name="Mystery" Address="1/2/5" />
    </GroupRange>
  </GroupRange>
</GroupAddress-Export>"#;

fn server(gateway: Option<&FakeGateway>, allow_write: bool) -> Server {
    let table = GroupAddressTable::parse(PROJECT).unwrap();
    let link = gateway.map(|g| Link::Tunnel {
        gateway: g.address().to_string(),
        transport: Transport::Udp,
    });
    Server::new(Bus::new(link, table, Some("test.xml".into())), allow_write)
}

async fn request(server: &Server, method: &str, params: Value) -> Value {
    let msg = json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params });
    let response = server.handle(&msg.to_string()).await.unwrap();
    assert_eq!(response["id"], 7);
    response
}

/// Call a tool; returns (isError, structured result or error text).
async fn call(server: &Server, name: &str, arguments: Value) -> (bool, Value) {
    let response = request(
        server,
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
    .await;
    let result = &response["result"];
    let is_error = result["isError"].as_bool().unwrap();
    if is_error {
        (true, result["content"][0]["text"].clone())
    } else {
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, result["structuredContent"]);
        (false, text)
    }
}

fn tool_names(list: &Value) -> Vec<&str> {
    list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn initialize_negotiates_version() {
    let s = server(None, false);
    let r = request(&s, "initialize", json!({ "protocolVersion": "2025-06-18" })).await;
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r["result"]["serverInfo"]["name"], "knxide-mcp");
    assert!(r["result"]["capabilities"]["tools"].is_object());

    let r = request(&s, "initialize", json!({ "protocolVersion": "1999-01-01" })).await;
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
}

#[tokio::test]
async fn protocol_errors() {
    let s = server(None, false);
    let r = s.handle("{not json").await.unwrap();
    assert_eq!(r["error"]["code"], -32700);
    assert_eq!(r["id"], Value::Null);

    let r = request(&s, "resources/list", json!({})).await;
    assert_eq!(r["error"]["code"], -32601);

    let r = request(&s, "tools/call", json!({ "name": "nope" })).await;
    assert_eq!(r["error"]["code"], -32602);

    // Notifications get no answer.
    let n = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    assert!(s.handle(&n.to_string()).await.is_none());

    let r = request(&s, "ping", json!({})).await;
    assert_eq!(r["result"], json!({}));
}

#[tokio::test]
async fn write_tool_only_when_allowed() {
    let r = request(&server(None, false), "tools/list", json!({})).await;
    assert!(!tool_names(&r).contains(&"write_group_address"));
    assert!(tool_names(&r).contains(&"read_group_address"));

    let s = server(None, false);
    let r = request(
        &s,
        "tools/call",
        json!({ "name": "write_group_address", "arguments": { "address": "1/2/3", "value": "on" } }),
    )
    .await;
    assert_eq!(r["error"]["code"], -32602);
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--allow-write")
    );

    let r = request(&server(None, true), "tools/list", json!({})).await;
    assert!(tool_names(&r).contains(&"write_group_address"));
}

#[tokio::test]
async fn find_matches_every_word() {
    let s = server(None, false);
    let (err, r) = call(
        &s,
        "find_group_addresses",
        json!({ "query": "living LIGHT" }),
    )
    .await;
    assert!(!err);
    assert_eq!(r["total"], 1);
    let entry = &r["group_addresses"][0];
    assert_eq!(entry["address"], "1/2/3");
    assert_eq!(entry["dpt"], "1.001");
    assert_eq!(entry["accepts"], "off or on");
    assert_eq!(entry["path"], json!(["Climate", "Living"]));

    let (_, r) = call(&s, "find_group_addresses", json!({ "query": "climate" })).await;
    assert_eq!(r["total"], 3);
    let (_, r) = call(&s, "find_group_addresses", json!({ "limit": 1 })).await;
    assert_eq!(r["total"], 3);
    assert_eq!(r["returned"], 1);

    let (err, msg) = call(&s, "find_group_addresses", json!({ "limit": 0 })).await;
    assert!(err, "{msg}");
}

#[tokio::test]
async fn no_gateway_is_a_tool_error() {
    let s = server(None, false);
    let (err, msg) = call(&s, "read_group_address", json!({ "address": "1/2/4" })).await;
    assert!(err);
    assert!(msg.as_str().unwrap().contains("no bus"), "{msg}");
}

#[tokio::test]
async fn read_decodes_with_project_dpt() {
    let gw = FakeGateway::spawn().unwrap();
    gw.set_value(ga!(1 / 2 / 4), Payload::Bytes(vec![0x0c, 0x1a])); // 21.00 °C
    let s = server(Some(&gw), false);

    let (err, r) = call(
        &s,
        "read_group_address",
        json!({ "address": "living temperature" }),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(r["address"], "1/2/4");
    assert_eq!(r["dpt"], "9.001");
    assert_eq!(r["value"], "21 °C");
    assert_eq!(r["raw"], "0c 1a");

    // No value on the bus: times out with an explanation.
    let (err, msg) = call(
        &s,
        "read_group_address",
        json!({ "address": "1/2/3", "timeout_ms": 200 }),
    )
    .await;
    assert!(err);
    assert!(
        msg.as_str().unwrap().contains("no device answered"),
        "{msg}"
    );

    let (_, status) = call(&s, "bus_status", json!({})).await;
    assert_eq!(status["connection"], "connected");
    assert_eq!(status["write_enabled"], false);
    s.bus().close().await;
}

#[tokio::test]
async fn write_encodes_and_sends() {
    let gw = FakeGateway::spawn().unwrap();
    let s = server(Some(&gw), true);

    let (err, r) = call(
        &s,
        "write_group_address",
        json!({ "address": "1/2/3", "value": "on" }),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(r["written"], "on");
    assert_eq!(gw.value(ga!(1 / 2 / 3)), Some(Payload::Small(1)));
    assert!(gw.received().iter().any(|c| matches!(
        c,
        Cemi::LDataReq(l) if l.apdu() == Some(&Apdu::GroupValueWrite(Payload::Small(1)))
    )));

    // Explicit DPT on an address without one.
    let (err, r) = call(
        &s,
        "write_group_address",
        json!({ "address": "Mystery", "value": "50", "dpt": "5.001" }),
    )
    .await;
    assert!(!err, "{r}");
    assert_eq!(gw.value(ga!(1 / 2 / 5)), Some(Payload::Bytes(vec![0x80])));

    // Without a DPT the value must be raw.
    let (err, msg) = call(
        &s,
        "write_group_address",
        json!({ "address": "1/2/5", "value": "50%" }),
    )
    .await;
    assert!(err);
    assert!(msg.as_str().unwrap().contains("raw"), "{msg}");

    // A bad value says what is accepted.
    let (err, msg) = call(
        &s,
        "write_group_address",
        json!({ "address": "1/2/3", "value": "maybe" }),
    )
    .await;
    assert!(err);
    assert!(msg.as_str().unwrap().contains("off or on"), "{msg}");
    s.bus().close().await;
}

#[tokio::test]
async fn telegrams_are_recorded_and_shown_as_last_value() {
    let gw = FakeGateway::spawn().unwrap();
    let s = server(Some(&gw), false);
    s.bus().client().await.unwrap();

    gw.indicate(&LData::group(
        ga!(1 / 2 / 4),
        Apdu::GroupValueWrite(Payload::Bytes(vec![0x0c, 0x1a])),
    ));
    let telegrams = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_, r) = call(&s, "recent_telegrams", json!({ "address": "1/2/4" })).await;
            if !r["telegrams"].as_array().unwrap().is_empty() {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let t = &telegrams["telegrams"][0];
    assert_eq!(t["service"], "write");
    assert_eq!(t["value"], "21 °C");
    assert_eq!(t["name"], "Living temperature");

    let (_, r) = call(&s, "find_group_addresses", json!({ "query": "1/2/4" })).await;
    assert_eq!(r["group_addresses"][0]["last_value"], "21 °C");
    s.bus().close().await;
}

#[tokio::test]
async fn serves_lines_over_a_stream() {
    let s = Arc::new(server(None, false));
    let (client, server_end) = tokio::io::duplex(64 * 1024);
    let (server_read, server_write) = tokio::io::split(server_end);
    let task = tokio::spawn(s.serve(BufReader::new(server_read), server_write));

    let (client_read, mut client_write) = tokio::io::split(client);
    let mut lines = BufReader::new(client_read).lines();
    client_write
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n\
              {\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\
              {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
        )
        .await
        .unwrap();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let line = lines.next_line().await.unwrap().unwrap();
        let msg: Value = serde_json::from_str(&line).unwrap();
        ids.push(msg["id"].as_i64().unwrap());
    }
    ids.sort();
    assert_eq!(ids, [1, 2]);

    client_write.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

/// A KNX IP router reached by unicast on loopback; the protocol is the same as multicast.
struct FakeRouter {
    socket: std::net::UdpSocket,
}

impl FakeRouter {
    fn new() -> Self {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self { socket }
    }

    /// The next routing indication and its sender.
    fn receive(&self) -> (LData, std::net::SocketAddr) {
        let mut buf = [0u8; 512];
        let (len, source) = self.socket.recv_from(&mut buf).unwrap();
        let Ok(Frame::RoutingIndication { cemi }) = Frame::decode(&buf[..len]) else {
            panic!("not a routing indication")
        };
        let Ok(Cemi::LDataInd(ldata)) = Cemi::decode(&cemi) else {
            panic!("not L_Data.ind")
        };
        (ldata, source)
    }

    fn indicate(&self, ldata: LData, to: std::net::SocketAddr) {
        let frame = Frame::RoutingIndication {
            cemi: Cemi::LDataInd(ldata).to_vec().unwrap(),
        };
        self.socket.send_to(&frame.to_vec().unwrap(), to).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn routing_reads_and_writes() {
    let router = FakeRouter::new();
    let mut options = RoutingOptions {
        bind: "127.0.0.1:0".parse().unwrap(),
        ..RoutingOptions::default()
    };
    options.router.group = router.socket.local_addr().unwrap();
    let table = GroupAddressTable::parse(PROJECT).unwrap();
    let s = Arc::new(Server::new(
        Bus::new(Some(Link::Routing(options)), table, None),
        true,
    ));

    // A read goes out as a routing indication; the answer comes back as one.
    let reader = {
        let s = s.clone();
        tokio::spawn(
            async move { call(&s, "read_group_address", json!({ "address": "1/2/4" })).await },
        )
    };
    let (read, client) = tokio::task::block_in_place(|| router.receive());
    assert_eq!(read.apdu(), Some(&Apdu::GroupValueRead));
    assert_eq!(read.source.to_string(), "15.15.250");
    let mut answer = LData::group(
        ga!(1 / 2 / 4),
        Apdu::GroupValueResponse(Payload::Bytes(vec![0x0c, 0x1a])),
    );
    answer.source = "1.1.10".parse().unwrap();
    router.indicate(answer, client);
    let (err, r) = reader.await.unwrap();
    assert!(!err, "{r}");
    assert_eq!(r["value"], "21 °C");
    assert_eq!(r["source"], "1.1.10");

    let (err, r) = call(
        &s,
        "write_group_address",
        json!({ "address": "Living light", "value": "on" }),
    )
    .await;
    assert!(!err, "{r}");
    let (write, _) = tokio::task::block_in_place(|| router.receive());
    assert_eq!(
        write.apdu(),
        Some(&Apdu::GroupValueWrite(Payload::Small(1)))
    );

    let (_, status) = call(&s, "bus_status", json!({})).await;
    assert_eq!(status["mode"], "routing");
    assert_eq!(status["connection"], "connected");
    assert_eq!(status["individual_address"], "15.15.250");

    // Our own frames show up in the history (local echo), as on a tunnel.
    let r = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (_, r) = call(&s, "recent_telegrams", json!({ "address": "1/2/3" })).await;
            if !r["telegrams"].as_array().unwrap().is_empty() {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(r["telegrams"][0]["value"], "on");
    s.bus().close().await;
}
