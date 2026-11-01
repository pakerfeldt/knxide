//! The dashboard server end to end against the fake gateway.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use knxide_core::{Apdu, LData, Payload, ga};
use knxide_dashboard::app::App;
use knxide_dashboard::config::{Dashboard, Kind, Page, Tile};
use knxide_dashboard::router;
use knxide_project::GroupAddressTable;
use knxide_testkit::FakeGateway;
use knxide_tokio::TunnelOptions;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const WAIT: Duration = Duration::from_secs(5);

const PROJECT: &str = r#"<GroupAddress-Export>
  <GroupRange Name="Belysning"><GroupRange Name="Kök">
    <GroupAddress Name="Kök tak bryt" Address="1/1/10" DPTs="DPST-1-1" />
    <GroupAddress Name="Kök tak status" Address="1/1/12" DPTs="DPST-1-1" />
    <GroupAddress Name="Kök tak dim absolut" Address="1/1/14" DPTs="DPST-5-1" />
  </GroupRange></GroupRange>
  <GroupRange Name="Värme"><GroupRange Name="Kök">
    <GroupAddress Name="Kök temperatur" Address="2/1/10" DPTs="DPST-9-1" />
    <GroupAddress Name="Börvärde kök" Address="2/1/11" DPTs="DPST-9-1" />
  </GroupRange></GroupRange>
</GroupAddress-Export>"#;

struct Tab {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    unread: Vec<Value>,
}

impl Tab {
    async fn next(&mut self, kind: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let matches = |m: &Value| m["type"] == kind && pred(m);
        if let Some(i) = self.unread.iter().position(matches) {
            return self.unread.remove(i);
        }
        tokio::time::timeout(WAIT, async {
            loop {
                let Some(Ok(Message::Text(text))) = self.ws.next().await else {
                    panic!("closed")
                };
                let msg: Value = serde_json::from_str(&text).unwrap();
                if matches(&msg) {
                    return msg;
                }
                self.unread.push(msg);
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no {kind}"))
    }

    async fn send(&mut self, msg: Value) {
        self.ws
            .send(Message::Text(msg.to_string().into()))
            .await
            .unwrap();
    }
}

async fn serve(gateway: &FakeGateway, file: Option<std::path::PathBuf>) -> (String, App) {
    let project = GroupAddressTable::parse(PROJECT).unwrap();
    let dashboard = Dashboard {
        title: "Test".into(),
        language: "en".into(),
        pages: vec![Page {
            id: "p".into(),
            name: "Kök".into(),
            tiles: vec![
                Tile {
                    id: "a".into(),
                    label: "Tak".into(),
                    group: Some("Kök".into()),
                    kind: Kind::Switch {
                        ga: "1/1/10".into(),
                        status: Some("1/1/12".into()),
                    },
                },
                Tile {
                    id: "b".into(),
                    label: "Temp".into(),
                    group: None,
                    kind: Kind::Value {
                        ga: "2/1/10".into(),
                    },
                },
            ],
        }],
    };
    let app = App::new(gateway.address().to_string(), project, dashboard, file);
    let options = TunnelOptions {
        bind: "127.0.0.1:0".parse().unwrap(),
        local_echo: true,
        ..TunnelOptions::default()
    };
    tokio::spawn(app.clone().run_bus(gateway.address(), options));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(axum::serve(listener, router(app.clone())).into_future());
    (addr, app)
}

/// The value of `ga` from the snapshot, or from the next update if it was not there yet.
async fn value_of(tab: &mut Tab, hello: &Value, ga: &str) -> Value {
    if !hello["values"][ga].is_null() {
        return hello["values"][ga].clone();
    }
    tab.next("value", |m| m["ga"] == ga).await["value"].clone()
}

async fn open(addr: &str) -> Tab {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    Tab {
        ws,
        unread: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dashboard_end_to_end() {
    let gateway = FakeGateway::spawn().unwrap();
    // Values the simulated devices report when read at start-up.
    gateway.set_value(ga!(1 / 1 / 12), Payload::Small(0));
    gateway.set_value(ga!(2 / 1 / 10), Payload::Bytes(vec![0x0c, 0x1a]));
    let dir = std::env::temp_dir().join(format!("knxide-dashboard-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("dashboard.json");
    let (addr, _app) = serve(&gateway, Some(file.clone())).await;
    let mut tab = open(&addr).await;
    let hello = tab.next("hello", |_| true).await;
    assert_eq!(hello["dashboard"]["title"], "Test");
    assert_eq!(hello["project"].as_array().unwrap().len(), 5);

    // Shown values are read at start-up. They may already be in the snapshot.
    if hello["state"]["status"] != "connected" {
        tab.next("state", |m| m["state"]["status"] == "connected")
            .await;
    }
    let temp = value_of(&mut tab, &hello, "2/1/10").await;
    assert_eq!(temp["text"], "21 °C");
    assert_eq!(temp["value"], 21.0);
    assert_eq!(value_of(&mut tab, &hello, "1/1/12").await["value"], false);

    // Switching: the write reaches the bus and our own frame updates the value.
    tab.send(json!({ "type": "write", "id": 1, "ga": "1/1/10", "value": "1", "dpt": "1.001" }))
        .await;
    assert_eq!(tab.next("result", |m| m["id"] == 1).await["ok"], true);
    assert_eq!(gateway.value(ga!(1 / 1 / 10)), Some(Payload::Small(1)));
    let echoed = tab.next("value", |m| m["ga"] == "1/1/10").await;
    assert_eq!(echoed["value"]["text"], "on");

    // Bus traffic updates values.
    let mut ldata = LData::group(ga!(1 / 1 / 12), Apdu::GroupValueWrite(Payload::Small(1)));
    ldata.source = "1.1.20".parse().unwrap();
    gateway.indicate(&ldata);
    assert_eq!(
        tab.next("value", |m| m["ga"] == "1/1/12").await["value"]["value"],
        true
    );

    // Suggestions pair addresses by name.
    tab.send(json!({ "type": "suggest", "id": 2, "path": ["Belysning"] }))
        .await;
    let suggested = tab.next("result", |m| m["id"] == 2).await;
    let tiles = suggested["tiles"].as_array().unwrap();
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0]["type"], "dimmer");
    assert_eq!(tiles[0]["label"], "Kök tak");
    assert_eq!(tiles[0]["switch_status"], "1/1/12");

    // Saving writes the file and reaches every tab.
    let mut other = open(&addr).await;
    other.next("hello", |_| true).await;
    let mut dashboard = hello["dashboard"].clone();
    dashboard["pages"][0]["tiles"]
        .as_array_mut()
        .unwrap()
        .push(tiles[0].clone());
    tab.send(json!({ "type": "save", "id": 3, "dashboard": dashboard }))
        .await;
    assert_eq!(tab.next("result", |m| m["id"] == 3).await["ok"], true);
    let pushed = other.next("dashboard", |_| true).await;
    assert_eq!(
        pushed["dashboard"]["pages"][0]["tiles"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved["pages"][0]["tiles"][2]["type"], "dimmer");

    // Bad input is refused.
    let mut bad = dashboard.clone();
    bad["pages"][0]["tiles"][0]["ga"] = json!("99/9/9");
    tab.send(json!({ "type": "save", "id": 4, "dashboard": bad }))
        .await;
    assert_eq!(tab.next("result", |m| m["id"] == 4).await["ok"], false);
    tab.send(json!({ "type": "write", "id": 5, "ga": "2/1/11", "value": "warm" }))
        .await;
    assert_eq!(tab.next("result", |m| m["id"] == 5).await["ok"], false);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_page_and_refuses_cross_site_sockets() {
    let gateway = FakeGateway::spawn().unwrap();
    let (addr, _app) = serve(&gateway, None).await;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for path in ["/", "/app.js", "/manifest.webmanifest", "/icon.svg"] {
        let mut stream = TcpStream::connect(&addr).await.unwrap();
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).await.unwrap();
        assert!(body.starts_with("HTTP/1.1 200"), "{path}");
    }
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = format!("ws://{addr}/ws").into_client_request().unwrap();
    request
        .headers_mut()
        .insert("origin", "http://evil.example".parse().unwrap());
    assert!(tokio_tungstenite::connect_async(request).await.is_err());
}
