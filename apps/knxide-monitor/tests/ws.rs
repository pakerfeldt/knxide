//! The server end to end: a WebSocket client drives it against the fake gateway.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use knxide_core::{Apdu, LData, Payload, ga};
use knxide_monitor::{Bus, Transport, app};
use knxide_project::GroupAddressTable;
use knxide_testkit::FakeGateway;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const WAIT: Duration = Duration::from_secs(5);
type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const PROJECT: &str = r#"<GroupAddress-Export xmlns="http://knx.org/xml/ga-export/01">
  <GroupRange Name="Climate" RangeStart="2048" RangeEnd="4095">
    <GroupRange Name="Living" RangeStart="2048" RangeEnd="2303">
      <GroupAddress Name="Living temperature" Address="1/2/4" DPTs="DPST-9-1" />
      <GroupAddress Name="Living light" Address="1/2/3" DPTs="DPST-1-1" />
    </GroupRange>
  </GroupRange>
</GroupAddress-Export>"#;

async fn serve() -> String {
    let table = GroupAddressTable::parse(PROJECT).unwrap();
    let bus = Bus::new(None, Transport::Udp, table, Some("test.xml".into()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app(bus)).await.unwrap() });
    addr.to_string()
}

/// A WebSocket that keeps messages it was not asked for, so order does not matter.
struct Tab {
    ws: Ws,
    unread: Vec<Value>,
}

async fn open(server: &str) -> Tab {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{server}/ws"))
        .await
        .unwrap();
    Tab {
        ws,
        unread: Vec::new(),
    }
}

/// Next message whose `type` is `kind` and that passes `pred`.
async fn next(tab: &mut Tab, kind: &str, pred: impl Fn(&Value) -> bool) -> Value {
    next_within(tab, kind, pred, WAIT).await
}

async fn next_within(
    tab: &mut Tab,
    kind: &str,
    pred: impl Fn(&Value) -> bool,
    wait: Duration,
) -> Value {
    let matches = |m: &Value| m["type"] == kind && pred(m);
    if let Some(i) = tab.unread.iter().position(matches) {
        return tab.unread.remove(i);
    }
    tokio::time::timeout(wait, async {
        loop {
            let Some(Ok(Message::Text(text))) = tab.ws.next().await else {
                panic!("socket closed")
            };
            let msg: Value = serde_json::from_str(&text).unwrap();
            if matches(&msg) {
                return msg;
            }
            tab.unread.push(msg);
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {kind} message"))
}

async fn send(tab: &mut Tab, msg: Value) {
    tab.ws
        .send(Message::Text(msg.to_string().into()))
        .await
        .unwrap();
}

#[tokio::test]
async fn console_end_to_end() {
    let gateway = FakeGateway::spawn().unwrap();
    let server = serve().await;
    let mut ws = open(&server).await;

    let hello = next(&mut ws, "hello", |_| true).await;
    assert_eq!(hello["state"]["status"], "disconnected");
    assert_eq!(hello["project"]["file"], "test.xml");
    let entries = hello["project"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    let light = entries.iter().find(|e| e["address"] == "1/2/3").unwrap();
    assert_eq!(
        light["kind"],
        json!({ "kind": "bool", "off": "off", "on": "on" })
    );
    assert_eq!(light["path"], json!(["Climate", "Living"]));

    send(
        &mut ws,
        json!({ "type": "connect", "gateway": gateway.address().to_string(), "transport": "udp" }),
    )
    .await;
    let connected = next(&mut ws, "state", |m| m["state"]["status"] == "connected").await;
    assert!(connected["state"]["address"].is_string());

    // Write with the project's DPT; our own frame comes back as a telegram.
    send(
        &mut ws,
        json!({ "type": "write", "id": 1, "address": "1/2/4", "value": "21.5" }),
    )
    .await;
    let result = next(&mut ws, "result", |m| m["id"] == 1).await;
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["message"], "Wrote 21.5 °C to 1/2/4");
    let own = next(&mut ws, "telegram", |m| m["telegram"]["own"] == true).await;
    assert_eq!(own["telegram"]["value"], "21.5 °C");
    assert_eq!(own["telegram"]["name"], "Living temperature");
    assert_eq!(
        gateway.value(ga!(1 / 2 / 4)),
        Some(Payload::Bytes(vec![0x0C, 0x33]))
    );

    // Read: the simulated device answers.
    send(
        &mut ws,
        json!({ "type": "read", "id": 2, "address": "1/2/4" }),
    )
    .await;
    let result = next(&mut ws, "result", |m| m["id"] == 2).await;
    assert_eq!(result["message"], "1/2/4 = 21.5 °C (from 1.1.10)");

    // Bus traffic is decoded and named.
    let mut ldata = LData::group(ga!(1 / 2 / 3), Apdu::GroupValueWrite(Payload::Small(1)));
    ldata.source = "1.1.5".parse().unwrap();
    gateway.indicate(&ldata);
    let t = next(&mut ws, "telegram", |m| {
        m["telegram"]["destination"] == "1/2/3"
    })
    .await;
    assert_eq!(t["telegram"]["value"], "on");
    assert_eq!(t["telegram"]["service"], "write");
    assert_eq!(t["telegram"]["source"], "1.1.5");

    // Health: a value that does not fit its DPT, and an address outside the project.
    let mut bad = LData::group(
        ga!(1 / 2 / 3),
        Apdu::GroupValueWrite(Payload::Bytes(vec![1, 2])),
    );
    bad.source = "1.1.6".parse().unwrap();
    gateway.indicate(&bad);
    let t = next(&mut ws, "telegram", |m| m["telegram"]["source"] == "1.1.6").await;
    assert_eq!(t["telegram"]["warning"], "2 bytes does not fit DPT 1.001");
    let mut stray = LData::group(ga!(5 / 0 / 9), Apdu::GroupValueWrite(Payload::Small(0)));
    stray.source = "1.1.20".parse().unwrap();
    gateway.indicate(&stray);
    let health = next(&mut ws, "health", |m| {
        let keys: Vec<&str> = m["issues"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|i| i["key"].as_str())
            .collect();
        keys.contains(&"dpt/1/2/3") && keys.contains(&"unknown/5/0/9")
    })
    .await;
    let mismatch = health["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == "dpt/1/2/3")
        .unwrap();
    assert_eq!(mismatch["severity"], "warning");
    assert_eq!(mismatch["device"], "1.1.6");
    send(
        &mut ws,
        json!({ "type": "dismiss_issue", "key": "dpt/1/2/3" }),
    )
    .await;
    next(&mut ws, "health", |m| {
        !m["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["key"] == "dpt/1/2/3")
    })
    .await;

    // Errors come back as results.
    send(
        &mut ws,
        json!({ "type": "write", "id": 3, "address": "1/2/4", "value": "warm" }),
    )
    .await;
    let result = next(&mut ws, "result", |m| m["id"] == 3).await;
    assert_eq!(result["ok"], false);
    send(
        &mut ws,
        json!({ "type": "write", "id": 4, "address": "7/7/7", "value": "0x0102" }),
    )
    .await;
    assert_eq!(next(&mut ws, "result", |m| m["id"] == 4).await["ok"], true);

    // A second tab sees the shared state and history.
    let mut other = open(&server).await;
    let hello = next(&mut other, "hello", |_| true).await;
    assert_eq!(hello["state"]["status"], "connected");
    assert!(hello["history"].as_array().unwrap().len() >= 3);
    // Devices heard so far, with what they sent to.
    let devices = hello["devices"].as_array().unwrap();
    let sensor = devices.iter().find(|d| d["address"] == "1.1.5").unwrap();
    assert_eq!(sensor["frames"], 1);
    assert_eq!(sensor["groups"], json!([["1/2/3", 1]]));

    // Restart: the simulated device accepts; an absent one is reported.
    send(
        &mut ws,
        json!({ "type": "restart", "id": 10, "address": "1.1.10" }),
    )
    .await;
    let result = next(&mut ws, "result", |m| m["id"] == 10).await;
    assert_eq!(result["message"], "Restart sent to 1.1.10", "{result}");
    assert_eq!(gateway.restarts(), ["1.1.10".parse().unwrap()]);
    send(
        &mut ws,
        json!({ "type": "restart", "id": 11, "address": "1.1.99" }),
    )
    .await;
    let result = next(&mut ws, "result", |m| m["id"] == 11).await;
    assert_eq!(result["ok"], false);
    assert_eq!(result["message"], "no device answers at 1.1.99");

    // Loading a project broadcasts it.
    let csv = "\"Group name\";\"Address\";\"DatapointType\"\n\"Kitchen\";\"5/0/1\";\"DPST-1-1\"\n";
    send(
        &mut ws,
        json!({ "type": "load_project", "id": 5, "name": "ga.csv", "content": csv }),
    )
    .await;
    let project = next(&mut other, "project", |_| true).await;
    assert_eq!(project["project"]["entries"][0]["name"], "Kitchen");

    // One-click DPT override and reset.
    send(
        &mut ws,
        json!({ "type": "set_dpt", "id": 20, "address": "5/0/1", "dpt": "14" }),
    )
    .await;
    assert_eq!(
        next(&mut ws, "result", |m| m["id"] == 20).await["message"],
        "5/0/1 now decodes as DPT 14"
    );
    let project = next(&mut other, "project", |m| {
        m["project"]["entries"][0]["overridden"] == true
    })
    .await;
    assert_eq!(project["project"]["entries"][0]["dpt"], "14");
    assert_eq!(project["project"]["entries"][0]["project_dpt"], "1.001");
    send(
        &mut ws,
        json!({ "type": "set_dpt", "id": 21, "address": "5/0/1", "dpt": null }),
    )
    .await;
    assert_eq!(
        next(&mut ws, "result", |m| m["id"] == 21).await["message"],
        "5/0/1 uses the project's DPT 1.001 again"
    );
    send(
        &mut ws,
        json!({ "type": "set_dpt", "id": 22, "address": "5/0/1", "dpt": "19.001" }),
    )
    .await;
    assert_eq!(
        next(&mut ws, "result", |m| m["id"] == 22).await["ok"],
        false
    );

    send(&mut ws, json!({ "type": "disconnect" })).await;
    next(&mut other, "state", |m| {
        m["state"]["status"] == "disconnected"
    })
    .await;
}

#[tokio::test]
async fn refuses_cross_origin_websockets() {
    let server = serve().await;
    let mut request = format!("ws://{server}/ws").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", "http://evil.example".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(request).await.is_err());

    let mut request = format!("ws://{server}/ws").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", format!("http://{server}").parse().unwrap());
    assert!(tokio_tungstenite::connect_async(request).await.is_ok());
}

#[tokio::test]
async fn serves_the_page() {
    let server = serve().await;
    let mut stream = TcpStream::connect(&server).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut body = String::new();
    stream.read_to_string(&mut body).await.unwrap();
    assert!(body.starts_with("HTTP/1.1 200"));
    assert!(body.contains("KNX bus console"));
}

#[tokio::test]
async fn connect_failure_is_reported() {
    let server = serve().await;
    let mut ws = open(&server).await;
    next(&mut ws, "hello", |_| true).await;
    send(
        &mut ws,
        json!({ "type": "connect", "gateway": "not an ip", "transport": "udp" }),
    )
    .await;
    // Bad input never leaves "disconnected"; a refused gateway reports an error state.
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    send(&mut ws, json!({ "type": "connect", "gateway": silent.local_addr().unwrap().to_string(), "transport": "udp" })).await;
    next(&mut ws, "state", |m| m["state"]["status"] == "connecting").await;
    // The default connect timeout is 10 s.
    let failed = next_within(
        &mut ws,
        "state",
        |m| m["state"]["status"] == "disconnected",
        Duration::from_secs(15),
    )
    .await;
    assert!(
        failed["state"]["error"]
            .as_str()
            .unwrap()
            .contains("no response"),
        "{failed}"
    );
}

#[tokio::test]
async fn overrides_are_saved_and_loaded() {
    let dir = std::env::temp_dir().join(format!("knxide-monitor-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("knx.overrides.json");
    let _ = std::fs::remove_file(&file);

    let bus = Bus::new(
        None,
        Transport::Udp,
        GroupAddressTable::parse(PROJECT).unwrap(),
        None,
    );
    assert_eq!(bus.use_overrides_file(file.clone()), Ok(0));
    bus.set_dpt("1/2/4", Some("14")).unwrap();
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved, json!({ "1/2/4": "14" }));

    let again = Bus::new(
        None,
        Transport::Udp,
        GroupAddressTable::parse(PROJECT).unwrap(),
        None,
    );
    assert_eq!(again.use_overrides_file(file.clone()), Ok(1));
    std::fs::remove_dir_all(dir).unwrap();
}
