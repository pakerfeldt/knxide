//! The server end to end: the JSON API and the WebSocket, with the test kit's made-up project
//! and simulated devices. No bus is involved.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use knxide_core::IndividualAddress;
use knxide_program::transport::TransportConfig;
use knxide_programmer::interfaces::Kind;
use knxide_programmer::link::BusAccess;
use knxide_programmer::ops::{CompareLimits, DownloadLimits};
use knxide_programmer::session::SessionConfig;
use knxide_programmer::sim::Faults;
use knxide_programmer::{Bus, Programmer, Settings, app, load_project};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;

const WAIT: Duration = Duration::from_secs(10);
const DIMMER: &str = "P-0001-0_DI-1";
const SENSOR: &str = "P-0001-0_DI-3";

/// A fresh place for this test's files.
fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        "knxide-programmer-test-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&path);
    path
}

/// The made-up project, written as a `.knxproj` file and read back the way the binary does.
fn project() -> knxide_program::Project {
    let path = scratch("demo.knxproj");
    std::fs::write(&path, knxide_testkit::project::archive()).unwrap();
    let project = load_project(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    project
}

fn fast() -> SessionConfig {
    SessionConfig {
        transport: TransportConfig {
            ack_timeout: Duration::from_millis(40),
            repetitions: 3,
            response_timeout: Duration::from_millis(200),
            lost_answer_timeout: None,
            unanswered_timeout: None,
        },
        response_retries: 1,
        stale_after: None,
    }
}

fn limits() -> CompareLimits {
    CompareLimits {
        reconnect_pause: Duration::from_millis(5),
        lost_answer_after: Some(Duration::from_millis(15)),
        ..CompareLimits::default()
    }
}

fn tool() -> IndividualAddress {
    "0.0.2".parse().unwrap()
}

async fn serve_with(bus: impl FnOnce(&knxide_program::Project) -> Bus) -> String {
    let project = project();
    let bus = bus(&project);
    let programmer = Programmer::with_timing(project, "demo.knxproj".into(), bus, fast(), limits());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app(programmer)).await.unwrap() });
    address.to_string()
}

/// A server on the made-up project with simulated devices that keeps its edits in `state`.
async fn serve_keeping(state: &std::path::Path) -> String {
    let project = project();
    let bus = Bus::simulate(&project, tool());
    let settings = Settings {
        session: fast(),
        limits: limits(),
        download: DownloadLimits::none(),
        state: Some(state.to_path_buf()),
    };
    let programmer = Programmer::open(project, "demo.knxproj".into(), bus, settings).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app(programmer)).await.unwrap() });
    address.to_string()
}

async fn serve() -> String {
    serve_with(|project| Bus::simulate(project, tool())).await
}

/// One HTTP request; returns the status and the body.
async fn http(
    server: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let mut stream = TcpStream::connect(server).await.unwrap();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {server}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let response = String::from_utf8(response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, body.to_string())
}

async fn get(server: &str, path: &str) -> (u16, Value) {
    let (status, body) = http(server, "GET", path, &[], "").await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

async fn post(server: &str, path: &str, body: Value) -> (u16, Value) {
    let (status, body) = http(server, "POST", path, &[], &body.to_string()).await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

async fn put(server: &str, path: &str, body: Value) -> (u16, Value) {
    let (status, body) = http(server, "PUT", path, &[], &body.to_string()).await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

async fn delete(server: &str, path: &str) -> (u16, Value) {
    let (status, body) = http(server, "DELETE", path, &[], "").await;
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

/// The parameters of an editor payload, in order, as `label=value as it reads`.
fn parameters(editor: &Value) -> Vec<String> {
    editor["pages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|page| page["items"].as_array().unwrap())
        .filter(|item| item["type"] == "parameter")
        .map(|item| {
            format!(
                "{}={}",
                item["label"].as_str().unwrap(),
                item["value_text"].as_str().unwrap()
            )
        })
        .collect()
}

/// The item of an editor payload with parameter reference `reference`.
fn item<'a>(editor: &'a Value, reference: &str) -> &'a Value {
    editor["pages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|page| page["items"].as_array().unwrap())
        .find(|item| item["reference"] == reference)
        .unwrap_or_else(|| panic!("no parameter {reference}"))
}

/// The links of every object of an editor payload, as `number: address address`.
fn links(editor: &Value) -> Vec<String> {
    editor["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|object| {
            let links: Vec<&str> = object["links"]
                .as_array()
                .unwrap()
                .iter()
                .map(|link| link["address"].as_str().unwrap())
                .collect();
            format!("{}: {}", object["number"], links.join(" "))
        })
        .collect()
}

/// Wait until the server's job is no longer running and return it.
async fn finished(server: &str, job: u64) -> Value {
    tokio::time::timeout(WAIT, async {
        loop {
            let (_, state) = get(server, "/api/job").await;
            if state["job"]["id"] == job && state["job"]["status"] != "running" {
                return state["job"].clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the job ends")
}

#[tokio::test(flavor = "multi_thread")]
async fn page_and_inventory() {
    let server = serve().await;
    let (status, page) = http(&server, "GET", "/", &[], "").await;
    assert_eq!(status, 200);
    assert!(page.contains("<title>knxide</title>"));
    assert!(
        !page.contains("http://") && !page.contains("https://"),
        "no outside resources"
    );
    for asset in ["/app.js", "/model.js", "/app.css"] {
        let (status, body) = http(&server, "GET", asset, &[], "").await;
        assert_eq!(status, 200, "{asset}");
        assert!(body.len() > 1000, "{asset}");
        assert!(!body.contains("https://"), "{asset}: no outside resources");
    }

    let (status, overview) = get(&server, "/api/project").await;
    assert_eq!(status, 200);
    assert_eq!(overview["project"]["name"], "Demo house");
    assert_eq!(overview["project"]["source"], "demo.knxproj");
    assert_eq!(overview["bus"]["mode"], "simulated");
    assert_eq!(overview["supported"], 3);
    let devices = overview["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 8);
    let dimmer = &devices[0];
    assert_eq!(dimmer["id"], DIMMER);
    assert_eq!(dimmer["address"], "1.1.9");
    assert_eq!(dimmer["sort"], 0x1109);
    assert_eq!(dimmer["name"], "Hall dimmer");
    assert_eq!(
        dimmer["location"],
        json!(["Demo house", "Ground floor", "Hall"])
    );
    assert_eq!(dimmer["order_number"], "D-100");
    assert_eq!(dimmer["product"], "Demo dimmer");
    assert_eq!(dimmer["application"]["name"], "Demo dimming 0102/0.3");
    assert_eq!(dimmer["application"]["mask"], "0021");
    assert_eq!(dimmer["application"]["version"], "0.3");
    assert_eq!(dimmer["supported"], true);
    assert_eq!(dimmer["state"], "project");
    assert_eq!(dimmer["edits"], 0);
    let plugin = &devices[3];
    assert_eq!(plugin["supported"], false);
    assert_eq!(plugin["obstacles"][0]["code"], "ui-plugin");
    // The reason in plain words, and what the planner said.
    assert_eq!(
        plugin["reason"],
        "Set up in the manufacturer's plug-in. Use ETS for this device."
    );
    assert_eq!(
        plugin["obstacles"][0]["text"],
        "plan may be wrong: object visibility is controlled by a manufacturer plugin"
    );
    assert_eq!(devices[7]["address"], Value::Null);
    assert_eq!(devices[7]["sort"], Value::Null);

    // The sensor: a partial download with the full one standing by.
    let (status, detail) = get(&server, &format!("/api/devices/{SENSOR}")).await;
    assert_eq!(status, 200);
    assert_eq!(detail["editable"], true);
    assert_eq!(detail["download"]["kind"], "partial");
    assert_eq!(detail["download"]["falls_back_to_full"], true);
    assert_eq!(detail["download"]["phases"][0], "Checking the device");
    assert_eq!(detail["capacity"]["addresses"], 4);
    assert_eq!(links(&detail), ["0: 1/0/1"]);
    assert_eq!(detail["objects"][0]["links"][0]["name"], "Living scene");
    assert_eq!(detail["objects"][0]["links"][0]["raw"], 2049);

    // Its technical details: tables, memory and requests of both kinds of download.
    let (status, technical) = get(&server, &format!("/api/devices/{SENSOR}/technical")).await;
    assert_eq!(status, 200);
    assert_eq!(technical["mask"], "0705");
    assert_eq!(technical["tables"]["addresses"][0]["address"], "1/0/1");
    assert_eq!(technical["tables"]["addresses"][0]["name"], "Living scene");
    assert_eq!(technical["tables"]["objects"][0]["links"][0], "1/0/1");
    assert!(technical["tables"]["objects"][0]["flags"]["communication"].is_boolean());
    let variants = technical["variants"].as_array().unwrap();
    assert_eq!(variants.len(), 2);
    assert_eq!(
        (&variants[0]["kind"], &variants[1]["kind"]),
        (&json!("partial"), &json!("full"))
    );
    assert!(variants[0]["bytes_sent"].as_u64() < variants[1]["bytes_sent"].as_u64());
    assert_eq!(variants[0]["requests"][0]["text"], "T_CONNECT");
    // One mark per byte of each segment.
    let segments = technical["segments"].as_array().unwrap();
    for (segment, sent) in segments.iter().zip(variants[0]["sent"].as_array().unwrap()) {
        assert_eq!(
            segment["data"].as_str().unwrap().len(),
            2 * sent.as_str().unwrap().len()
        );
    }

    let (status, plan) = get(&server, &format!("/api/devices/{SENSOR}/plan")).await;
    assert_eq!(status, 200);
    assert_eq!(plan["variants"][0]["requests"], variants[0]["requests"]);

    // A device with a dialog plug-in can be looked at, not edited; one without a plan has no
    // technical details.
    let (status, detail) = get(&server, "/api/devices/P-0001-0_DI-4").await;
    assert_eq!(status, 200);
    assert_eq!(detail["supported"], false);
    assert_eq!(detail["editable"], false);
    assert!(detail["objects"].is_array());
    for path in ["plan", "technical"] {
        let (status, refusal) = get(&server, &format!("/api/devices/P-0001-0_DI-6/{path}")).await;
        assert_eq!(status, 422);
        assert!(
            refusal["error"]
                .as_str()
                .unwrap()
                .contains("mask version 07B0")
        );
    }
    // A device without its application program: listed, with nothing to edit.
    let (status, detail) = get(&server, "/api/devices/P-0001-0_DI-7").await;
    assert_eq!(status, 200);
    assert_eq!(detail["pages"], Value::Null);
    assert_eq!(detail["obstacles"][0]["code"], "missing-application");
    let (status, _) = get(&server, "/api/devices/nobody").await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn compare_and_download() {
    let server = serve().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{server}/ws"))
        .await
        .unwrap();
    let Some(Ok(Message::Text(hello))) = socket.next().await else {
        panic!("no hello")
    };
    let hello: Value = serde_json::from_str(&hello).unwrap();
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["job"], Value::Null);

    // Compare: the simulated device is empty, so it differs from the plan.
    let (status, started) = post(
        &server,
        &format!("/api/devices/{DIMMER}/compare"),
        json!({}),
    )
    .await;
    assert_eq!(status, 202, "{started}");
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["status"], "succeeded", "{job}");
    assert_eq!(job["kind"], "compare");
    assert_eq!(job["result"]["kind"], "full");
    assert!(job["result"]["differing"].as_u64().unwrap() > 0);
    assert_eq!(job["result"]["application"], "00FA/0102 version 03");
    assert_eq!(job["result"]["complete"], true);
    assert_eq!(job["result"]["unread"], json!([]));
    assert_eq!(
        (&job["result"]["repetitions"], &job["result"]["reconnects"]),
        (&json!(0), &json!(0))
    );
    assert_eq!(
        job["result"]["matching"].as_u64().unwrap() + job["result"]["differing"].as_u64().unwrap(),
        job["result"]["compared"].as_u64().unwrap()
    );
    let first = &job["result"]["differences"][0];
    assert_eq!(
        first["planned"].as_str().unwrap().len(),
        first["device"].as_str().unwrap().len()
    );

    // A download needs one explicit confirmation, and nothing else will do.
    let path = format!("/api/devices/{DIMMER}/download");
    let (status, refusal) = post(&server, &path, json!({})).await;
    assert_eq!(status, 400);
    assert!(refusal["error"].as_str().unwrap().contains("confirm"));
    let (status, _) = post(&server, &path, json!({ "confirm": "1.1.9" })).await;
    assert_eq!(status, 400);
    let (status, _) = post(&server, &path, json!({ "confirm": false })).await;
    assert_eq!(status, 400);
    let (status, _) = http(&server, "POST", &path, &[], "confirm=true").await;
    assert_eq!(status, 400);
    let (status, started) = post(&server, &path, json!({ "confirm": true })).await;
    assert_eq!(status, 202, "{started}");
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["status"], "succeeded", "{job}");
    assert_eq!(job["result"]["kind"], "full");
    assert_eq!(job["sent"], job["result"]["requests"]);
    assert_eq!(job["expected"], job["sent"]);

    // Now the device holds what the plan writes.
    let (_, started) = post(
        &server,
        &format!("/api/devices/{DIMMER}/compare"),
        json!({}),
    )
    .await;
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["result"]["differing"], 0, "{job}");
    assert_eq!(job["result"]["findings"], json!([]));
    let (_, overview) = get(&server, "/api/project").await;
    assert_eq!(overview["devices"][0]["last"]["kind"], "compare");
    assert_eq!(overview["devices"][0]["last"]["result"]["differing"], 0);
    // The app downloaded what the device now shows.
    assert_eq!(overview["devices"][0]["state"], "downloaded");
    assert!(overview["devices"][0]["downloaded_ms"].as_u64().unwrap() > 0);

    // The socket carried it all: jobs, progress, and a log with every request and answer.
    let mut kinds = std::collections::BTreeMap::<String, usize>::new();
    let mut requests = Vec::new();
    let mut jobs_done = 0;
    // For each job: what it expected to send when it started, and what it sent.
    let mut totals: Vec<(String, u64, u64)> = Vec::new();
    while jobs_done < 3 {
        let message = tokio::time::timeout(WAIT, socket.next()).await.unwrap();
        let Some(Ok(Message::Text(text))) = message else {
            panic!("socket closed")
        };
        let message: Value = serde_json::from_str(&text).unwrap();
        *kinds
            .entry(message["type"].as_str().unwrap().to_string())
            .or_default() += 1;
        if message["type"] == "log" && message["line"]["level"] == "request" {
            requests.push(message["line"]["text"].as_str().unwrap().to_string());
        }
        if message["type"] == "job" {
            let job = &message["job"];
            if job["status"] == "running" {
                totals.push((
                    job["kind"].as_str().unwrap().to_string(),
                    job["expected"].as_u64().unwrap(),
                    0,
                ));
            } else {
                jobs_done += 1;
                // Finished: what was sent is all there was, so the bar is full.
                assert_eq!(job["expected"], job["sent"], "{job}");
                totals.last_mut().unwrap().2 = job["sent"].as_u64().unwrap();
            }
        }
        if message["type"] == "progress" {
            // Never more sent than expected while it runs, whichever total applies.
            assert!(
                message["expected"].is_null()
                    || message["expected"].as_u64() >= message["sent"].as_u64()
            );
        }
    }
    // A comparison has its own total: the pre-check and its reads, far fewer requests than
    // the download, and on a clean link exactly what it then sends.
    let kinds_run: Vec<&str> = totals.iter().map(|(kind, ..)| kind.as_str()).collect();
    assert_eq!(kinds_run, ["compare", "download", "compare"]);
    for (kind, expected, sent) in &totals {
        assert_eq!(expected, sent, "{kind}");
    }
    assert!(totals[0].1 < totals[1].1, "{totals:?}");
    assert_eq!(kinds["job"], 6, "{kinds:?}");
    assert!(kinds["progress"] > 60 && kinds["log"] > 120, "{kinds:?}");
    assert_eq!(requests[0], "T_CONNECT");
    assert!(requests.contains(&"RESTART".to_string()));
    assert_eq!(requests.last().unwrap(), "T_DISCONNECT");

    // The log of the last job, and every job with its log under Activity.
    let (_, state) = get(&server, "/api/job").await;
    let log = state["log"].as_array().unwrap();
    assert!(log.len() > 30, "{}", log.len());
    assert!(log.iter().all(|line| line["job"] == state["job"]["id"]));
    let (_, jobs) = get(&server, "/api/jobs").await;
    let jobs = jobs["jobs"].as_array().unwrap();
    let kinds: Vec<&str> = jobs
        .iter()
        .map(|job| job["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["compare", "download", "compare"], "newest first");
    let download = jobs[1]["id"].as_u64().unwrap();
    let (status, one) = get(&server, &format!("/api/jobs/{download}")).await;
    assert_eq!(status, 200);
    assert_eq!(one["job"]["kind"], "download");
    assert!(
        one["log"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line["text"] == "RESTART")
    );
    let (status, _) = get(&server, "/api/jobs/999").await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn partial_download_of_a_loaded_device() {
    let server = serve().await;
    let (status, started) = post(
        &server,
        &format!("/api/devices/{SENSOR}/download"),
        json!({ "confirm": true }),
    )
    .await;
    assert_eq!(status, 202, "{started}");
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["status"], "succeeded", "{job}");
    // The simulated device holds the application: the download stays partial.
    assert_eq!(job["result"]["kind"], "partial");
    assert_eq!(job["result"]["full_because"], Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn refusals() {
    let server = serve().await;
    // Not supported: nothing is started.
    let (status, refusal) = post(&server, "/api/devices/P-0001-0_DI-4/compare", json!({})).await;
    assert_eq!(status, 422);
    assert!(
        refusal["error"]
            .as_str()
            .unwrap()
            .contains("Set up in the manufacturer's plug-in")
    );
    let (status, _) = post(
        &server,
        "/api/devices/P-0001-0_DI-4/download",
        json!({ "confirm": true }),
    )
    .await;
    assert_eq!(status, 422);
    let (status, _) = post(&server, "/api/devices/nobody/compare", json!({})).await;
    assert_eq!(status, 404);

    // From another site: refused, whatever the body says.
    let path = format!("/api/devices/{DIMMER}/download");
    let body = json!({ "confirm": true }).to_string();
    let origin = [("Origin", "http://evil.example")];
    let (status, _) = http(&server, "POST", &path, &origin, &body).await;
    assert_eq!(status, 403);
    let (status, _) = http(&server, "POST", "/api/job/cancel", &origin, "").await;
    assert_eq!(status, 403);
    let own = format!("http://{server}");
    let (status, _) = http(&server, "POST", &path, &[("Origin", &own)], &body).await;
    assert_eq!(status, 202);
    let (_, state) = get(&server, "/api/job").await;
    finished(&server, state["job"]["id"].as_u64().unwrap()).await;

    // Without a bus the page is for looking only.
    let server = serve_with(|_| Bus::None).await;
    let (_, overview) = get(&server, "/api/project").await;
    assert_eq!(overview["bus"]["mode"], "none");
    let (status, refusal) = post(
        &server,
        &format!("/api/devices/{DIMMER}/compare"),
        json!({}),
    )
    .await;
    assert_eq!(status, 409);
    assert!(refusal["error"].as_str().unwrap().contains("no bus"));
    let (status, _) = get(&server, &format!("/api/devices/{DIMMER}/plan")).await;
    assert_eq!(status, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_operation_at_a_time_and_cancel() {
    // A device that takes no frame: every operation on it waits for acknowledgements.
    let server = serve_with(|project| {
        let bus = Bus::simulate(project, tool());
        if let Bus::Simulated(simulated, _) = &bus {
            simulated.with("1.1.9".parse().unwrap(), |endpoint| {
                endpoint.faults = Faults {
                    lose_requests: u32::MAX,
                    ..Faults::default()
                };
            });
        }
        bus
    })
    .await;
    let compare = format!("/api/devices/{DIMMER}/compare");
    let (status, started) = post(&server, &compare, json!({})).await;
    assert_eq!(status, 202);
    let (status, refusal) = post(&server, &compare, json!({})).await;
    assert_eq!(status, 409);
    assert_eq!(refusal["error"], "another operation is running");
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["status"], "failed");
    assert!(
        job["error"]
            .as_str()
            .unwrap()
            .contains("did not acknowledge"),
        "{job}"
    );
    assert_eq!(job["check"], Value::Null);

    // Cancelled while it waits.
    let (_, started) = post(&server, &compare, json!({})).await;
    let (status, cancelled) = post(&server, "/api/job/cancel", json!({})).await;
    assert_eq!((status, &cancelled["cancelled"]), (200, &json!(true)));
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["status"], "cancelled", "{job}");
    // Nothing to cancel any more.
    let (_, cancelled) = post(&server, "/api/job/cancel", json!({})).await;
    assert_eq!(cancelled["cancelled"], false);
}

/// A server whose dimmer (1.1.9) has `faults`.
async fn serve_faulty(faults: Faults) -> String {
    serve_with(move |project| {
        let bus = Bus::simulate(project, tool());
        if let Bus::Simulated(simulated, _) = &bus {
            simulated.with("1.1.9".parse().unwrap(), |endpoint| {
                endpoint.faults = faults
            });
        }
        bus
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_comparison_over_a_bad_link_shows_it() {
    // The device forgets its connection after twelve numbered frames: ten of the checking
    // phase (the pre-check and the three load states) and two reads. One new connection later
    // the comparison is complete.
    let server = serve_faulty(Faults {
        forget_connection_after: Some(12),
        ..Faults::default()
    })
    .await;
    let (_, started) = post(
        &server,
        &format!("/api/devices/{DIMMER}/compare"),
        json!({}),
    )
    .await;
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["status"], "succeeded", "{job}");
    assert_eq!(job["result"]["complete"], true);
    assert_eq!(job["result"]["reconnects"], 1);
    assert_eq!(job["reconnects"], 1);

    // The device takes twelve numbered frames and then nothing: the comparison gives up and
    // says what it has and what it could not read.
    let server = serve_faulty(Faults {
        silent_after: Some(12),
        ..Faults::default()
    })
    .await;
    let (_, started) = post(
        &server,
        &format!("/api/devices/{DIMMER}/compare"),
        json!({}),
    )
    .await;
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["status"], "incomplete", "{job}");
    assert_eq!(job["error"], Value::Null);
    let result = &job["result"];
    assert_eq!(result["complete"], false);
    assert!(
        result["stopped_because"]
            .as_str()
            .unwrap()
            .contains("failed 5 times in a row")
    );
    let compared = result["compared"].as_u64().unwrap();
    let unread = result["unread_bytes"].as_u64().unwrap();
    assert!(compared > 0 && unread > 0);
    assert_eq!(compared + unread, result["planned"].as_u64().unwrap());
    let stretches = result["unread"].as_array().unwrap();
    assert_eq!(
        stretches
            .iter()
            .map(|stretch| stretch["length"].as_u64().unwrap())
            .sum::<u64>(),
        unread
    );
    assert_eq!(result["reconnects"], 4);
    assert_eq!(job["reconnects"], 4);
    assert!(job["repetitions"].as_u64().unwrap() >= 12);
    assert_eq!(job["repetitions"], result["repetitions"]);
    let (_, overview) = get(&server, "/api/project").await;
    assert_eq!(overview["devices"][0]["last"]["status"], "incomplete");
}

#[tokio::test(flavor = "multi_thread")]
async fn answers_asked_for_again_are_counted_on_their_own() {
    // Every fourth exchange loses the device's acknowledgement and its answer, and the
    // device does not answer the repetition again: the comparison acknowledges the answer it
    // never saw and asks again, on the same connection.
    let server = serve_faulty(Faults {
        lose_exchange_every: Some(4),
        keeps_answers_to_itself: true,
        ..Faults::default()
    })
    .await;
    let (status, started) = post(
        &server,
        &format!("/api/devices/{DIMMER}/compare"),
        json!({}),
    )
    .await;
    assert_eq!(status, 202);
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["status"], "succeeded", "{job}");
    let again = job["re_requests"].as_u64().unwrap();
    assert!(again >= 3, "{job}");
    assert_eq!(job["reconnects"], 0);
    assert_eq!(job["repetitions"], again);
    assert_eq!(job["result"]["re_requests"], again);
    assert_eq!(job["expected"], job["sent"]);
    let (_, state) = get(&server, "/api/job").await;
    let notes = state["log"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|line| {
            line["level"] == "note"
                && line["text"]
                    .as_str()
                    .unwrap()
                    .contains("taking the answer as lost")
        })
        .count();
    assert_eq!(notes as u64, again);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unloaded_device_is_told_in_words() {
    // What a download leaves that stopped after its unloads.
    let server = serve_with(|project| {
        let bus = Bus::simulate(project, tool());
        if let Bus::Simulated(simulated, _) = &bus {
            simulated.with("1.1.9".parse().unwrap(), |endpoint| {
                for object in 1..=3 {
                    endpoint.device_mut().set_load_state(object, 0);
                }
                endpoint.device_mut().refuse_memory_reads(true);
            });
        }
        bus
    })
    .await;
    let (_, started) = post(
        &server,
        &format!("/api/devices/{DIMMER}/compare"),
        json!({}),
    )
    .await;
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["status"], "succeeded", "{job}");
    assert_eq!(job["reconnects"], 0);
    let result = &job["result"];
    assert_eq!(
        result["not_loaded"],
        "the device holds no application: its address table, association table and \
         application are unloaded; it needs a full download"
    );
    assert_eq!(
        result["load_states"],
        json!({ "address_table": 0, "association_table": 0, "application": 0 })
    );
    assert_eq!(
        (&result["compared"], &result["findings"]),
        (&json!(0), &json!([]))
    );
    assert_eq!(job["expected"], job["sent"]);
    // No memory was asked for, and the device page has the result to show.
    let (_, state) = get(&server, "/api/job").await;
    assert!(!state["log"].as_array().unwrap().iter().any(|line| {
        line["level"] == "request" && line["text"].as_str().unwrap().starts_with("MEM_READ")
    }));
    let (_, editor) = get(&server, &format!("/api/devices/{DIMMER}")).await;
    assert_eq!(editor["last"]["result"]["not_loaded"], result["not_loaded"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_that_stops_says_how_far_it_got() {
    // After 20 numbered frames the device is gone for good, in the middle of the procedure:
    // the download tries to find out what the device holds, cannot, and stops.
    let server = serve_faulty(Faults {
        silent_after: Some(20),
        ..Faults::default()
    })
    .await;
    let (status, started) = post(
        &server,
        &format!("/api/devices/{DIMMER}/download"),
        json!({ "confirm": true }),
    )
    .await;
    assert_eq!(status, 202);
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["status"], "failed", "{job}");
    let error = job["error"].as_str().unwrap();
    assert!(
        error.contains("did not acknowledge the request") && error.contains("recovering from that"),
        "{error}"
    );
    // One attempt to recover, on one new connection.
    assert_eq!(
        (&job["recoveries"], &job["reconnects"]),
        (&json!(1), &json!(1))
    );
    let progress = &job["progress"];
    assert!(progress["writes"].as_u64().unwrap() > 0);
    assert!(progress["steps_completed"].as_u64().unwrap() < progress["steps"].as_u64().unwrap());
    assert!(progress["failed_step"].is_string());
    let last = progress["last_write"].as_str().unwrap();
    assert!(last.starts_with("MEM_WRITE") || last.starts_with("PROP_WRITE"));
    let written = job["written"].as_str().unwrap();
    assert!(
        written.contains(last) && written.contains("partly loaded"),
        "{written}"
    );
    // The log's last line says the same, and no write went out twice.
    let (_, state) = get(&server, "/api/job").await;
    let log = state["log"].as_array().unwrap();
    let last_line = log.last().unwrap();
    assert_eq!(last_line["level"], "error");
    assert!(
        last_line["text"]
            .as_str()
            .unwrap()
            .contains("partly loaded")
    );
    let mut writes: Vec<&str> = log
        .iter()
        .filter(|line| line["level"] == "request")
        .map(|line| line["text"].as_str().unwrap())
        .filter(|text| text.starts_with("MEM_WRITE") || text.starts_with("PROP_WRITE"))
        .collect();
    let sent = writes.len();
    writes.dedup();
    assert_eq!(writes.len(), sent);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_recovers_from_a_lost_connection_by_reading() {
    // The device drops its connection after 20 numbered frames, in the middle of the
    // procedure. The download opens a new one, reads what the request under way should have
    // left, finds it not done, sends it, and goes on to the end.
    let server = serve_faulty(Faults {
        forget_connection_after: Some(20),
        ..Faults::default()
    })
    .await;
    let (_, started) = post(
        &server,
        &format!("/api/devices/{DIMMER}/download"),
        json!({ "confirm": true }),
    )
    .await;
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["status"], "succeeded", "{job}");
    assert_eq!(
        (&job["recoveries"], &job["reconnects"]),
        (&json!(1), &json!(1))
    );
    assert_eq!(job["result"]["recoveries"], 1);
    let (_, state) = get(&server, "/api/job").await;
    let log = state["log"].as_array().unwrap();
    assert!(log.iter().any(|line| {
        line["level"] == "note"
            && line["text"]
                .as_str()
                .unwrap()
                .contains("reading what the device holds instead of stopping")
    }));
    // The device now holds what the download writes.
    let (_, started) = post(
        &server,
        &format!("/api/devices/{DIMMER}/compare"),
        json!({}),
    )
    .await;
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["result"]["differing"], 0, "{job}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_check_is_named() {
    // The device at the dimmer's address reports another manufacturer.
    let server = serve_with(|project| {
        let bus = Bus::simulate(project, tool());
        if let Bus::Simulated(simulated, _) = &bus {
            simulated.with("1.1.9".parse().unwrap(), |endpoint| {
                endpoint.device_mut().set_property(0, 12, 1, &[0x00, 0x02]);
            });
        }
        bus
    })
    .await;
    for (path, body) in [
        (format!("/api/devices/{DIMMER}/compare"), json!({})),
        (
            format!("/api/devices/{DIMMER}/download"),
            json!({ "confirm": true }),
        ),
    ] {
        let (status, started) = post(&server, &path, body).await;
        assert_eq!(status, 202);
        let job = finished(&server, started["job"].as_u64().unwrap()).await;
        assert_eq!(job["status"], "failed");
        assert_eq!(job["check"], "manufacturer");
        assert_eq!(
            job["error"],
            "manufacturer check failed: the device reports 0002, the application is for 00FA"
        );
        assert_eq!(job["written"], "nothing was written to the device");
    }
    let (_, state) = get(&server, "/api/job").await;
    let log = state["log"].as_array().unwrap();
    assert!(!log.iter().any(|line| {
        let text = line["text"].as_str().unwrap();
        line["level"] == "request"
            && (text.starts_with("MEM_WRITE") || text.starts_with("PROP_WRITE"))
    }));
    assert!(log.iter().any(|line| line["level"] == "error"));
}

#[tokio::test]
async fn projects_from_disk() {
    // An unpacked project: the same inventory, and the baggage is not touched.
    let directory = scratch("unpacked");
    for (path, content) in knxide_testkit::project::files() {
        let file = directory.join(&path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, content).unwrap();
    }
    std::fs::write(directory.join("stray.txt"), "not part of a project").unwrap();
    let project = load_project(&directory).unwrap();
    assert_eq!(project.devices.len(), 8);
    assert_eq!(project.name, "Demo house");
    assert_eq!(
        project
            .devices
            .iter()
            .filter(|device| device.is_supported())
            .count(),
        3
    );
    std::fs::remove_dir_all(&directory).unwrap();

    // A protected project is refused with a message that says so.
    let protected = scratch("protected.knxproj");
    std::fs::write(
        &protected,
        knxide_testkit::zip::stored(&[("P-0001.zip".to_string(), b"PK".to_vec())]),
    )
    .unwrap();
    let error = load_project(&protected).unwrap_err();
    assert!(error.contains("password-protected"), "{error}");
    std::fs::remove_file(&protected).unwrap();

    // Not an archive at all, and a file that is not there.
    let other = scratch("notes.knxproj");
    std::fs::write(&other, "just text").unwrap();
    assert!(load_project(&other).unwrap_err().contains("ZIP"));
    std::fs::remove_file(&other).unwrap();
    assert!(load_project(&scratch("missing.knxproj")).is_err());
}

// ------------------------------------------------------------------------------------------
// Editing
// ------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn parameters_are_edited_and_visibility_follows() {
    let server = serve().await;
    let device = format!("/api/devices/{DIMMER}");
    let (_, editor) = get(&server, &device).await;
    // One page, from the product's channel and block, with its texts; the parameter nobody
    // may see is not there, the separator is a line of text.
    let page = &editor["pages"][0];
    assert_eq!(
        (&page["channel"], &page["title"]),
        (&json!("Main"), &json!("General"))
    );
    assert_eq!(
        parameters(&editor),
        [
            "Level=6",
            "Mode > all=Dimming",
            "Hardware revision=7",
            "High=nine"
        ]
    );
    assert!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "text"
                && item["text"] == "What follows depends on the mode.")
    );
    let level = item(&editor, "P-1_R-1");
    assert_eq!(
        level["control"],
        json!({ "type": "number", "minimum": 0, "maximum": 255 })
    );
    assert_eq!(level["suffix"], "%");
    assert_eq!(level["standing"], "default");
    assert_eq!(level["default_text"], "6");
    let mode = item(&editor, "P-9_R-9");
    assert_eq!(
        mode["standing"], "project",
        "Dimming in the project, Switching by default"
    );
    assert_eq!(mode["default_text"], "Switching");
    let choices: Vec<&str> = mode["control"]["choices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|choice| choice["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        choices,
        ["Off", "Switching", "Dimming", "Dimming with scenes"]
    );
    assert_eq!(item(&editor, "P-6_R-6")["control"]["type"], "fixed");
    assert_eq!(item(&editor, "UP-2_R-2")["standing"], "default");
    assert_eq!(links(&editor), ["0: 1/2/2", "1: 1/1/1"]);

    // What cannot be: out of range, not a number, read-only, unknown, hidden.
    let level_path = format!("{device}/parameters/P-1_R-1");
    let (status, refusal) = put(&server, &level_path, json!({ "value": "300" })).await;
    assert_eq!(status, 422);
    assert!(
        refusal["error"].as_str().unwrap().contains("0 to 255"),
        "{refusal}"
    );
    let (status, _) = put(&server, &level_path, json!({ "value": "many" })).await;
    assert_eq!(status, 422);
    let (status, _) = put(&server, &level_path, json!({})).await;
    assert_eq!(status, 400);
    let (status, _) = put(
        &server,
        &format!("{device}/parameters/P-6_R-6"),
        json!({ "value": "8" }),
    )
    .await;
    assert_eq!(status, 422, "read-only");
    let (status, _) = put(
        &server,
        &format!("{device}/parameters/P-7_R-7"),
        json!({ "value": "8" }),
    )
    .await;
    assert_eq!(status, 422, "not accessible");
    let (status, _) = put(
        &server,
        &format!("{device}/parameters/P-77_R-77"),
        json!({ "value": "8" }),
    )
    .await;
    assert_eq!(status, 422, "no such parameter");
    let mode_path = format!("{device}/parameters/P-9_R-9");
    let (status, _) = put(&server, &mode_path, json!({ "value": "7" })).await;
    assert_eq!(status, 422, "not one of the choices");
    let (_, editor) = get(&server, &device).await;
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("project"), &json!(0))
    );

    // Switching brings in another parameter and a third group object, at once.
    let (status, editor) = put(&server, &mode_path, json!({ "value": "1" })).await;
    assert_eq!(status, 200, "{editor}");
    assert_eq!(
        parameters(&editor),
        [
            "Level=6",
            "Mode > all=Switching",
            "Hardware revision=7",
            "Low=three"
        ]
    );
    assert_eq!(links(&editor), ["0: 1/2/2", "1: 1/1/1", "2: "]);
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("edited"), &json!(1))
    );
    let mode = item(&editor, "P-9_R-9");
    assert_eq!(mode["standing"], "edited");
    assert_eq!(
        (&mode["value"], &mode["baseline_text"]),
        (&json!("1"), &json!("Dimming"))
    );
    assert_eq!(
        editor["changes"]["parameters"],
        json!([{ "reference": "P-9_R-9", "page": "General", "label": "Mode > all", "from": "Dimming", "to": "Switching" }])
    );
    assert_eq!(editor["changes"]["gained"][0]["number"], 2);
    assert_eq!(editor["changes"]["dropped"], json!([]));
    // The list shows the device as edited.
    let (_, overview) = get(&server, "/api/project").await;
    assert_eq!(overview["devices"][0]["state"], "edited");

    // Off: the third object goes again, and yet another parameter shows.
    let (_, editor) = put(&server, &mode_path, json!({ "value": "0" })).await;
    assert_eq!(
        parameters(&editor),
        [
            "Level=6",
            "Mode > all=Off",
            "Hardware revision=7",
            "Spare=200"
        ]
    );
    assert_eq!(links(&editor), ["0: 1/2/2", "1: 1/1/1"]);

    // A second edit, then one of them reset to the project's value.
    let (status, editor) = put(&server, &level_path, json!({ "value": "40" })).await;
    assert_eq!(status, 200);
    assert_eq!(editor["edits"], 2);
    let (status, editor) = delete(&server, &mode_path).await;
    assert_eq!(status, 200);
    assert_eq!(
        parameters(&editor),
        [
            "Level=40",
            "Mode > all=Dimming",
            "Hardware revision=7",
            "High=nine"
        ]
    );
    assert_eq!(editor["edits"], 1);
    // Setting a value back to the project's is no edit either.
    let (_, editor) = put(&server, &level_path, json!({ "value": "6" })).await;
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("project"), &json!(0))
    );

    // A device that cannot be programmed here is not edited here.
    let (status, refusal) = put(
        &server,
        "/api/devices/P-0001-0_DI-4/parameters/P-1_R-1",
        json!({ "value": "1" }),
    )
    .await;
    assert_eq!(status, 422);
    assert!(
        refusal["error"].as_str().unwrap().contains("Use ETS"),
        "{refusal}"
    );
    let (status, _) = put(
        &server,
        "/api/devices/nobody/parameters/P-1_R-1",
        json!({ "value": "1" }),
    )
    .await;
    assert_eq!(status, 404);
    // Not from another site.
    let (status, _) = http(
        &server,
        "PUT",
        &level_path,
        &[("Origin", "http://evil.example")],
        r#"{"value":"9"}"#,
    )
    .await;
    assert_eq!(status, 403);
}

#[tokio::test(flavor = "multi_thread")]
async fn links_and_group_addresses_are_edited() {
    let server = serve().await;
    let device = format!("/api/devices/{DIMMER}");
    let object = format!("{device}/objects/O-0_R-1");

    // A second address, put first: it becomes the sending one.
    let (status, editor) = put(&server, &object, json!({ "links": ["1/1/1", "1/2/2"] })).await;
    assert_eq!(status, 200, "{editor}");
    assert_eq!(links(&editor), ["0: 1/1/1 1/2/2", "1: 1/1/1"]);
    let change = &editor["changes"]["links"][0];
    assert_eq!(
        (&change["number"], &change["sending_changed"]),
        (&json!(0), &json!(true))
    );
    assert_eq!(change["added"][0]["address"], "1/1/1");
    assert_eq!(change["added"][0]["name"], "Hall light switch");
    assert_eq!(change["removed"], json!([]));
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("edited"), &json!(1))
    );

    // What cannot be: not an address, the same address twice, more than the tables hold.
    let (status, _) = put(&server, &object, json!({ "links": ["nonsense"] })).await;
    assert_eq!(status, 400);
    let (status, _) = put(&server, &object, json!({ "links": "1/1/1" })).await;
    assert_eq!(status, 400);
    let (status, refusal) = put(
        &server,
        &object,
        json!({ "links": ["1/0/1", "1/1/1", "1/1/2", "1/1/3", "1/2/2", "2/2/52", "2/2/53", "2/2/54"] }),
    )
    .await;
    assert_eq!(status, 422, "{refusal}");
    let message = refusal["error"].as_str().unwrap();
    assert!(
        message.contains("does not fit the device") && message.contains("tables"),
        "{message}"
    );
    let (status, _) = put(
        &server,
        &format!("{device}/objects/O-9_R-9"),
        json!({ "links": [] }),
    )
    .await;
    assert_eq!(status, 422, "no such object");
    // Nothing of that stuck.
    let (_, editor) = get(&server, &device).await;
    assert_eq!(links(&editor), ["0: 1/1/1 1/2/2", "1: 1/1/1"]);

    // A new group address, made here.
    let (status, refusal) = post(
        &server,
        "/api/group-addresses",
        json!({ "address": "40/1/1", "name": "x" }),
    )
    .await;
    assert_eq!(status, 422, "{refusal}");
    let (status, _) = post(
        &server,
        "/api/group-addresses",
        json!({ "address": "3/0/7", "name": " " }),
    )
    .await;
    assert_eq!(status, 422);
    let (status, _) = post(
        &server,
        "/api/group-addresses",
        json!({ "address": "3/0/7", "name": "Hall night light", "datapoint_type": "one bit" }),
    )
    .await;
    assert_eq!(status, 422);
    let (status, created) = post(
        &server,
        "/api/group-addresses",
        json!({ "address": "3/0/7", "name": "Hall night light", "datapoint_type": "DPST-1-1" }),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["address"], "3/0/7");
    let (status, _) = post(
        &server,
        "/api/group-addresses",
        json!({ "address": "3/0/7", "name": "Again" }),
    )
    .await;
    assert_eq!(status, 409);
    let (status, _) = post(
        &server,
        "/api/group-addresses",
        json!({ "address": "1/1/1", "name": "Taken" }),
    )
    .await;
    assert_eq!(status, 409);

    // Linked, it shows by name everywhere; an object without links is simply unlinked.
    let (status, editor) = put(&server, &object, json!({ "links": ["3/0/7"] })).await;
    assert_eq!(status, 200);
    assert_eq!(editor["objects"][0]["links"][0]["name"], "Hall night light");
    let (status, editor) = put(
        &server,
        &format!("{device}/objects/O-1_R-2"),
        json!({ "links": [] }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(links(&editor), ["0: 3/0/7", "1: "]);
    let (_, technical) = get(&server, &format!("{device}/technical")).await;
    assert_eq!(
        technical["tables"]["addresses"],
        json!([{ "address": "3/0/7", "raw": 0x1807, "name": "Hall night light", "datapoint_type": "DPST-1-1", "connection": 1 }])
    );

    // The address list: where each address comes from and who uses it.
    let (_, renamed) = put(
        &server,
        "/api/group-addresses/2305",
        json!({ "name": "Hall ceiling" }),
    )
    .await;
    assert_eq!(renamed["saved"], true);
    // An address made here needs a name; one nobody knows cannot be renamed.
    let (status, _) = put(&server, "/api/group-addresses/6151", json!({ "name": "" })).await;
    assert_eq!(status, 422);
    let (status, _) = put(&server, "/api/group-addresses/999", json!({ "name": "x" })).await;
    assert_eq!(status, 404);
    let (status, list) = get(&server, "/api/group-addresses").await;
    assert_eq!(status, 200);
    let all = list["addresses"].as_array().unwrap();
    assert_eq!(all.len(), 9);
    let find = |address: &str| {
        all.iter()
            .find(|entry| entry["address"] == address)
            .unwrap()
    };
    let made = find("3/0/7");
    assert_eq!(made["source"], "created");
    assert_eq!(made["used_by"][0]["device"], DIMMER);
    assert_eq!(made["used_by"][0]["device_name"], "Hall dimmer");
    assert_eq!(
        (
            &made["used_by"][0]["number"],
            &made["used_by"][0]["sending"]
        ),
        (&json!(0), &json!(true))
    );
    let hall = find("1/1/1");
    assert_eq!(
        (&hall["source"], &hall["name"]),
        (&json!("renamed"), &json!("Hall ceiling"))
    );
    assert_eq!(hall["project_name"], "Hall light switch");
    assert_eq!(hall["used_by"], json!([]), "unlinked above");
    assert_eq!(find("1/0/1")["used_by"][0]["device"], SENSOR);
    assert_eq!(find("2/2/53")["source"], "project");
    let ranges = list["ranges"].as_array().unwrap();
    assert!(
        ranges
            .iter()
            .any(|range| range["name"] == "Lighting" && range["level"] == 0)
    );
    assert!(
        ranges
            .iter()
            .any(|range| range["name"] == "Valves" && range["level"] == 1)
    );

    // Everything dropped: the device is as the project has it. The made address stays.
    let (status, editor) = delete(&server, &format!("{device}/edits")).await;
    assert_eq!(status, 200);
    assert_eq!(links(&editor), ["0: 1/2/2", "1: 1/1/1"]);
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("project"), &json!(0))
    );
    // No name given for a project's address: its own name is back.
    put(&server, "/api/group-addresses/2305", json!({ "name": "" })).await;
    let (_, list) = get(&server, "/api/group-addresses").await;
    let all = list["addresses"].as_array().unwrap();
    assert_eq!(all.len(), 9);
    let hall = all
        .iter()
        .find(|entry| entry["address"] == "1/1/1")
        .unwrap();
    assert_eq!(
        (&hall["source"], &hall["name"]),
        (&json!("project"), &json!("Hall light switch"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_are_kept_beside_the_project() {
    let directory = scratch("state");
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("demo.knxproj.knxide.json");
    let device = format!("/api/devices/{DIMMER}");
    let level = format!("{device}/parameters/P-1_R-1");

    let server = serve_keeping(&file).await;
    assert!(
        !file.exists(),
        "nothing is written before something is edited"
    );
    let (_, overview) = get(&server, "/api/project").await;
    assert_eq!(overview["state_file"], file.display().to_string());
    let (status, editor) = put(&server, &level, json!({ "value": "40" })).await;
    assert_eq!((status, &editor["saved"]), (200, &json!(true)));
    put(
        &server,
        &format!("{device}/objects/O-0_R-1"),
        json!({ "links": ["1/1/1", "1/2/2"] }),
    )
    .await;
    post(
        &server,
        "/api/group-addresses",
        json!({ "address": "3/0/7", "name": "Hall night light" }),
    )
    .await;
    put(
        &server,
        "/api/group-addresses/2305",
        json!({ "name": "Hall ceiling" }),
    )
    .await;

    // The file: written whole (no temporary file is left), and plain to read.
    let names: Vec<String> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["demo.knxproj.knxide.json"]);
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(kept["project"], "P-0001");
    assert_eq!(kept["devices"][DIMMER]["parameters"]["P-1_R-1"], "40");
    assert_eq!(
        kept["devices"][DIMMER]["links"]["O-0_R-1"],
        json!(["1/1/1", "1/2/2"])
    );

    // Another start of the program finds it all again.
    let server = serve_keeping(&file).await;
    let (_, editor) = get(&server, &device).await;
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("edited"), &json!(2))
    );
    assert_eq!(item(&editor, "P-1_R-1")["value"], "40");
    assert_eq!(links(&editor), ["0: 1/1/1 1/2/2", "1: 1/1/1"]);
    assert_eq!(editor["objects"][0]["links"][0]["name"], "Hall ceiling");
    let (_, list) = get(&server, "/api/group-addresses").await;
    assert!(
        list["addresses"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["address"] == "3/0/7" && entry["name"] == "Hall night light")
    );

    // Downloaded: the device counts as downloaded for as long as nothing else is changed,
    // also after a restart of the program.
    let (status, started) = post(
        &server,
        &format!("{device}/download"),
        json!({ "confirm": true }),
    )
    .await;
    assert_eq!(status, 202, "{started}");
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["status"], "succeeded", "{job}");
    let (_, editor) = get(&server, &device).await;
    assert_eq!(editor["state"], "downloaded");
    let when = editor["downloaded_ms"].as_u64().unwrap();
    let server = serve_keeping(&file).await;
    let (_, editor) = get(&server, &device).await;
    assert_eq!(
        (&editor["state"], &editor["downloaded_ms"]),
        (&json!("downloaded"), &json!(when))
    );
    let (_, editor) = put(&server, &level, json!({ "value": "41" })).await;
    assert_eq!(editor["state"], "edited");
    let (_, editor) = put(&server, &level, json!({ "value": "40" })).await;
    assert_eq!(editor["state"], "downloaded");

    // Discarded: back to what was programmed, not to the project, in the file too.
    put(&server, &level, json!({ "value": "41" })).await;
    let (status, editor) = delete(&server, &format!("{device}/edits")).await;
    assert_eq!((status, &editor["saved"]), (200, &json!(true)));
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("downloaded"), &json!(0))
    );
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(kept["devices"][DIMMER]["parameters"]["P-1_R-1"], "40");
    assert_eq!(
        kept["devices"][DIMMER]["downloaded"]["configuration"]["parameters"]["P-1_R-1"],
        "40"
    );
    let server = serve_keeping(&file).await;
    let (_, editor) = get(&server, &device).await;
    assert_eq!(item(&editor, "P-1_R-1")["value"], "40");
    assert_eq!(editor["state"], "downloaded");

    // A file that is not this project's, or not readable, stops the start: nothing is
    // overwritten on a guess.
    let open = |path: &std::path::Path| {
        let settings = Settings {
            session: fast(),
            limits: limits(),
            download: DownloadLimits::none(),
            state: Some(path.to_path_buf()),
        };
        Programmer::open(project(), "demo.knxproj".into(), Bus::None, settings).map(|_| ())
    };
    let other = directory.join("other.json");
    std::fs::write(
        &other,
        std::fs::read_to_string(&file)
            .unwrap()
            .replace("P-0001", "P-0002"),
    )
    .unwrap();
    assert!(open(&other).unwrap_err().contains("P-0002"));
    std::fs::write(&other, "{ not json").unwrap();
    assert!(open(&other).is_err());
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "{ not json");
    std::fs::remove_dir_all(&directory).unwrap();
}

/// The bytes of the memory image where two technical views differ, as addresses.
fn differing_bytes(before: &Value, after: &Value) -> Vec<u64> {
    let mut out = Vec::new();
    let (before, after) = (
        before["segments"].as_array().unwrap(),
        after["segments"].as_array().unwrap(),
    );
    assert_eq!(before.len(), after.len());
    for (one, other) in before.iter().zip(after) {
        assert_eq!(one["address"], other["address"]);
        let (one_data, other_data) = (
            one["data"].as_str().unwrap(),
            other["data"].as_str().unwrap(),
        );
        assert_eq!(one_data.len(), other_data.len());
        for at in (0..one_data.len()).step_by(2) {
            if one_data[at..at + 2] != other_data[at..at + 2] {
                out.push(one["address"].as_u64().unwrap() + at as u64 / 2);
            }
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_changes_the_plan_and_a_comparison_names_it() {
    let server = serve().await;
    let device = format!("/api/devices/{DIMMER}");
    // The device holds what the project says.
    let (_, started) = post(
        &server,
        &format!("{device}/download"),
        json!({ "confirm": true }),
    )
    .await;
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["status"], "succeeded", "{job}");

    let (_, before) = get(&server, &format!("{device}/technical")).await;
    let (_, editor) = put(
        &server,
        &format!("{device}/parameters/P-1_R-1"),
        json!({ "value": "40" }),
    )
    .await;
    assert_eq!(editor["state"], "edited");
    let (_, after) = get(&server, &format!("{device}/technical")).await;
    // One byte of the image changed: the parameter's, at offset 11 of the segment at 0200.
    assert_eq!(differing_bytes(&before, &after), [0x020B]);
    assert_eq!(before["tables"], after["tables"]);
    assert_ne!(
        before["variants"][0]["requests"],
        after["variants"][0]["requests"]
    );
    let (_, plan) = get(&server, &format!("{device}/plan")).await;
    assert_eq!(plan["variants"], after["variants"]);

    // A link more: the tables change, and of the image only the two tables.
    let (_, editor) = put(
        &server,
        &format!("{device}/objects/O-0_R-1"),
        json!({ "links": ["1/2/2", "1/0/1"] }),
    )
    .await;
    assert_eq!(editor["edits"], 2);
    let (_, linked) = get(&server, &format!("{device}/technical")).await;
    let addresses: Vec<&str> = linked["tables"]["addresses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["address"].as_str().unwrap())
        .collect();
    assert_eq!(addresses, ["1/0/1", "1/1/1", "1/2/2"]);
    assert_eq!(
        linked["tables"]["associations"].as_array().unwrap().len(),
        3
    );
    let changed = differing_bytes(&after, &linked);
    assert!(!changed.is_empty());
    // The address table is at 0116 and the association table follows it, so that grows and
    // this moves; the pointer to the association table is at 0111.
    assert!(
        changed
            .iter()
            .all(|address| *address == 0x0111 || (0x0116..0x0132).contains(address)),
        "{changed:04X?}"
    );
    assert!(changed.contains(&0x0111));

    // The comparison says what differs in words: the parameter and the object.
    let (_, started) = post(&server, &format!("{device}/compare"), json!({})).await;
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["status"], "succeeded", "{job}");
    let findings = job["result"]["findings"].as_array().unwrap();
    assert_eq!(
        findings[0],
        json!({ "type": "parameter", "reference": "P-1_R-1", "page": "General", "label": "Level", "here": "40 %", "device": "6 %", "adoptable": true })
    );
    let object = findings
        .iter()
        .find(|finding| finding["type"] == "links")
        .unwrap();
    assert_eq!(object["number"], 0);
    let read = |list: &Value| -> Vec<String> {
        list.as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["address"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(read(&object["here"]), ["1/2/2", "1/0/1"]);
    assert_eq!(read(&object["device"]), ["1/2/2"]);
    assert!(
        findings.iter().all(|finding| finding["type"] != "bytes"),
        "every difference has a name: {findings:?}"
    );
    let (_, editor) = get(&server, &device).await;
    assert_eq!(editor["findings"].as_array().unwrap().len(), findings.len());

    // Adopting one difference takes the device's value for it; adopting all leaves nothing
    // edited, and the device counts as downloaded again.
    let (status, editor) = post(
        &server,
        &format!("{device}/adopt"),
        json!({ "references": ["P-1_R-1"] }),
    )
    .await;
    assert_eq!(status, 200, "{editor}");
    assert_eq!(item(&editor, "P-1_R-1")["value"], "6");
    assert_eq!(editor["edits"], 1);
    let (_, started) = post(&server, &format!("{device}/compare"), json!({})).await;
    finished(&server, started["job"].as_u64().unwrap()).await;
    let (status, editor) = post(&server, &format!("{device}/adopt"), json!({})).await;
    assert_eq!(status, 200, "{editor}");
    assert_eq!(links(&editor), ["0: 1/2/2", "1: 1/1/1"]);
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("downloaded"), &json!(0))
    );
    // Nothing to adopt without a comparison.
    let (status, _) = post(&server, &format!("/api/devices/{SENSOR}/adopt"), json!({})).await;
    assert_eq!(status, 409);
}

// ------------------------------------------------------------------------------------------
// The recorded devices
// ------------------------------------------------------------------------------------------

/// The value of attribute `name` of the first element of `text` that has it.
fn attribute<'a>(text: &'a str, name: &str) -> &'a str {
    let start = text
        .find(&format!(" {name}=\""))
        .unwrap_or_else(|| panic!("no attribute {name}"))
        + name.len()
        + 3;
    let rest = &text[start..];
    &rest[..rest.find('"').unwrap()]
}

/// A project directory around one recorded device: the device on a line, its group
/// addresses, a hardware entry that leads to its application program, and that program.
/// Returns the device's identifier.
fn project_around(recording: &std::path::Path, directory: &std::path::Path) -> String {
    let read = |file: &str| std::fs::read_to_string(recording.join(file)).unwrap();
    let (program, device) = (read("app.xml"), read("device.xml"));
    let device = device.trim_start_matches('\u{feff}');
    let application = attribute(
        &program[program.find("<ApplicationProgram ").unwrap()..],
        "Id",
    );
    let manufacturer = application.split('_').next().unwrap();
    let id = attribute(device, "Id").to_string();
    let project = id.split("-0_").next().unwrap();
    let product = attribute(device, "ProductRefId");
    let hardware = product.rsplit_once("_P-").unwrap().0;
    let link = attribute(device, "Hardware2ProgramRefId");
    let addresses: String = read("group-addresses.txt")
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(name, raw)| {
            format!(
                r#"<GroupAddress Id="{project}-0_{name}" Address="{}" Name="{name}" />"#,
                raw.trim()
            )
        })
        .collect();
    let write = |path: &str, content: String| {
        let file = directory.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, content).unwrap();
    };
    write(
        &format!("{project}/project.xml"),
        format!(
            r#"<KNX xmlns="http://knx.org/xml/project/20"><Project Id="{project}"><ProjectInformation Name="Recorded" /></Project></KNX>"#
        ),
    );
    write(
        &format!("{project}/0.xml"),
        format!(
            r#"<KNX xmlns="http://knx.org/xml/project/20"><Project Id="{project}"><Installations>
<Installation Name="" BCUKey="4294967295"><Topology><Area Address="1"><Line Address="1">
{device}
</Line></Area></Topology>
<GroupAddresses><GroupRanges><GroupRange Name="All" RangeStart="1" RangeEnd="65535">{addresses}</GroupRange></GroupRanges></GroupAddresses>
</Installation></Installations></Project></KNX>"#
        ),
    );
    write(
        &format!("{manufacturer}/Hardware.xml"),
        format!(
            r#"<KNX xmlns="http://knx.org/xml/project/20"><ManufacturerData><Manufacturer RefId="{manufacturer}"><Hardware>
<Hardware Id="{hardware}" Name="Recorded"><Products><Product Id="{product}" Text="Recorded" OrderNumber="R-1" /></Products>
<Hardware2Programs><Hardware2Program Id="{link}"><ApplicationProgramRef RefId="{application}" /></Hardware2Program></Hardware2Programs></Hardware>
</Hardware></Manufacturer></ManufacturerData></KNX>"#
        ),
    );
    write(&format!("{manufacturer}/{application}.xml"), program);
    id
}

/// Which parameters and objects an editor payload shows: what an edit must leave alone to
/// count as an edit that changes nothing else.
fn shape(editor: &Value) -> Vec<String> {
    let mut out: Vec<String> = editor["pages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|page| page["items"].as_array().unwrap())
        .map(|item| item["reference"].as_str().unwrap_or("text").to_string())
        .collect();
    for list in ["objects", "unused"] {
        out.extend(
            editor[list]
                .as_array()
                .unwrap()
                .iter()
                .map(|object| object["reference"].as_str().unwrap().to_string()),
        );
    }
    out
}

/// Start from each recorded device, change one enumeration parameter and one link through
/// the API, and check that the image and the tables change where they should and nowhere
/// else.
#[tokio::test(flavor = "multi_thread")]
async fn edits_to_recorded_devices_change_the_plan_where_they_should() {
    let Some(root) = std::env::var_os("KNXIDE_PROGRAM_FIXTURES").map(PathBuf::from) else {
        eprintln!("skipped: KNXIDE_PROGRAM_FIXTURES is not set");
        return;
    };
    let mut recordings: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("app.xml").is_file())
        .collect();
    recordings.sort();
    assert!(!recordings.is_empty());
    for recording in recordings {
        let name = recording
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let directory = scratch("recorded");
        let id = project_around(&recording, &directory);
        let project = load_project(&directory).unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
        let programmer =
            Programmer::with_timing(project, name.clone(), Bus::None, fast(), limits());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move { axum::serve(listener, app(programmer)).await.unwrap() });

        let device = format!("/api/devices/{id}");
        let (status, editor) = get(&server, &device).await;
        assert_eq!(status, 200, "{name}");
        assert_eq!(editor["editable"], true, "{name}: {}", editor["obstacles"]);
        let (_, before) = get(&server, &format!("{device}/technical")).await;

        // An enumeration parameter whose change shows or hides nothing and lands in memory.
        let candidates: Vec<(String, String)> = editor["pages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|page| page["items"].as_array().unwrap())
            .filter(|item| item["control"]["type"] == "choice")
            .filter_map(|item| {
                let other = item["control"]["choices"]
                    .as_array()?
                    .iter()
                    .find(|choice| choice["value"] != item["value"])?;
                Some((
                    item["reference"].as_str()?.to_string(),
                    other["value"].as_str()?.to_string(),
                ))
            })
            .collect();
        let mut edited = None;
        for (reference, value) in candidates {
            let path = format!("{device}/parameters/{reference}");
            let (status, after) = put(&server, &path, json!({ "value": value })).await;
            if status != 200 {
                continue;
            }
            let (_, technical) = get(&server, &format!("{device}/technical")).await;
            let changed = differing_bytes(&before, &technical);
            if shape(&after) == shape(&editor) && !changed.is_empty() {
                edited = Some((reference, after, technical, changed));
                break;
            }
            let (status, _) = delete(&server, &path).await;
            assert_eq!(status, 200);
        }
        let (reference, after, technical, changed) =
            edited.unwrap_or_else(|| panic!("{name}: no enumeration parameter to edit"));
        // The bytes of one small field changed, and nothing else: not the tables, not which
        // bytes are sent.
        let (first, last) = (changed[0], changed[changed.len() - 1]);
        assert!(
            last - first < 4,
            "{name}: {reference} changed {changed:04X?}"
        );
        assert_eq!(before["tables"], technical["tables"], "{name}");
        for (one, other) in before["variants"]
            .as_array()
            .unwrap()
            .iter()
            .zip(technical["variants"].as_array().unwrap())
        {
            assert_eq!(one["sent"], other["sent"], "{name}");
            assert_eq!(one["bytes_sent"], other["bytes_sent"], "{name}");
            assert_ne!(one["requests"], other["requests"], "{name}");
        }
        assert_eq!(
            (&after["state"], &after["edits"]),
            (&json!("edited"), &json!(1))
        );
        assert_eq!(
            after["changes"]["parameters"][0]["reference"],
            reference.as_str()
        );
        assert_eq!(item(&after, &reference)["standing"], "edited");
        // Reset, the plan is the project's again, byte for byte and request for request.
        let (_, reset) = delete(&server, &format!("{device}/parameters/{reference}")).await;
        assert_eq!(reset["state"], "project", "{name}");
        let (_, again) = get(&server, &format!("{device}/technical")).await;
        assert_eq!(again, before, "{name}");

        // One more group address on an object that has one already.
        let object = editor["objects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|object| !object["links"].as_array().unwrap().is_empty())
            .unwrap_or_else(|| panic!("{name}: no linked object"));
        let (status, _) = post(
            &server,
            "/api/group-addresses",
            json!({ "address": "31/7/254", "name": "Made for the test" }),
        )
        .await;
        assert_eq!(status, 201, "{name}");
        let mut wanted: Vec<Value> = object["links"]
            .as_array()
            .unwrap()
            .iter()
            .map(|link| link["address"].clone())
            .collect();
        wanted.push(json!("31/7/254"));
        let path = format!("{device}/objects/{}", object["reference"].as_str().unwrap());
        let (status, after) = put(&server, &path, json!({ "links": wanted })).await;
        assert_eq!(status, 200, "{name}: {after}");
        assert_eq!(
            after["changes"]["links"][0]["added"][0]["address"],
            "31/7/254"
        );
        let (_, linked) = get(&server, &format!("{device}/technical")).await;
        let count =
            |technical: &Value, table: &str| technical["tables"][table].as_array().unwrap().len();
        // The highest address there is: it goes last in the table, nothing else moves.
        assert_eq!(
            count(&linked, "addresses"),
            count(&before, "addresses") + 1,
            "{name}"
        );
        assert_eq!(
            linked["tables"]["addresses"][count(&before, "addresses")]["address"],
            "31/7/254",
            "{name}"
        );
        assert_eq!(
            linked["tables"]["addresses"].as_array().unwrap()[..count(&before, "addresses")],
            before["tables"]["addresses"].as_array().unwrap()[..],
            "{name}"
        );
        // One association more; where the table has a slot per object whether linked or
        // not, the new one takes a free slot instead.
        let free = |technical: &Value| {
            technical["tables"]["associations"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| entry["address"].is_null())
                .count()
        };
        assert_eq!(
            count(&linked, "associations") - free(&linked),
            count(&before, "associations") - free(&before) + 1,
            "{name}"
        );
        assert!(
            linked["tables"]["associations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["address"] == "31/7/254" && entry["object"] == object["number"]),
            "{name}"
        );
        // The object table is as before, but for the object's new link.
        let mut expected = before["tables"]["objects"].clone();
        for entry in expected.as_array_mut().unwrap() {
            if entry["number"] == object["number"] {
                entry["links"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("31/7/254"));
            }
        }
        assert_eq!(linked["tables"]["objects"], expected, "{name}");
        // Of the image, only the two tables changed (and, where the association table moves
        // with the size of the address table, the byte that says where it is).
        let place = |technical: &Value, table: &str| {
            u64::from_str_radix(technical["tables"][table].as_str().unwrap(), 16).unwrap()
        };
        let address_table = place(&linked, "address_table");
        let address_end = address_table + 3 + 2 * count(&linked, "addresses") as u64 + 1;
        let association_from =
            place(&before, "association_table").min(place(&linked, "association_table"));
        let association_end =
            place(&linked, "association_table") + 1 + 2 * count(&linked, "associations") as u64;
        let moved = place(&before, "association_table") != place(&linked, "association_table");
        let changed = differing_bytes(&before, &linked);
        assert!(!changed.is_empty(), "{name}");
        for address in &changed {
            assert!(
                (address_table..address_end).contains(address)
                    || (association_from..association_end).contains(address)
                    || (moved && *address == 0x0111),
                "{name}: byte {address:04X} changed; the tables are at {address_table:04X}..{address_end:04X} and {association_from:04X}..{association_end:04X}"
            );
        }
        eprintln!(
            "{name}: parameter {reference} changed {} byte(s), the link {} byte(s) in the tables",
            last - first + 1,
            changed.len()
        );
    }
}

// ------------------------------------------------------------------------------------------
// A product that only asks for a download plug-in
// ------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_download_plug_in_alone_leaves_editing_and_comparing() {
    const ACTUATOR: &str = "P-0001-0_DI-5";
    let server = serve().await;
    let (_, overview) = get(&server, "/api/project").await;
    // Three devices can be downloaded; one more can be edited and compared.
    assert_eq!(
        (&overview["supported"], &overview["editable"]),
        (&json!(3), &json!(4))
    );
    let rows = overview["devices"].as_array().unwrap();
    let row = rows.iter().find(|row| row["id"] == ACTUATOR).unwrap();
    assert_eq!(row["supported"], false);
    assert_eq!(
        (&row["editable"], &row["comparable"]),
        (&json!(true), &json!(true))
    );
    assert_eq!(row["obstacles"].as_array().unwrap().len(), 1);
    assert_eq!(row["obstacles"][0]["code"], "download-plugin");
    let blocked = row["download_blocked"].as_str().unwrap();
    assert_eq!(row["reason"], blocked);
    assert!(
        blocked.contains("download plug-in") && blocked.contains("ETS"),
        "{blocked}"
    );
    // The others are as they were: all three or none.
    for other in rows.iter().filter(|row| row["id"] != ACTUATOR) {
        assert_eq!(other["editable"], other["supported"], "{other}");
        assert_eq!(other["comparable"], other["supported"], "{other}");
        assert_eq!(other["download_blocked"], Value::Null, "{other}");
    }

    // The full editor: pages, objects, technical details, and edits that stick.
    let device = format!("/api/devices/{ACTUATOR}");
    let (status, editor) = get(&server, &device).await;
    assert_eq!(status, 200);
    assert_eq!(editor["editable"], true);
    assert!(editor["download"]["phases"].is_array());
    let number = editor["pages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|page| page["items"].as_array().unwrap())
        .find(|item| item["control"]["type"] == "number")
        .expect("a number parameter");
    let reference = number["reference"].as_str().unwrap();
    let other = if number["value"] == "7" { "8" } else { "7" };
    let (status, edited) = put(
        &server,
        &format!("{device}/parameters/{reference}"),
        json!({ "value": other }),
    )
    .await;
    assert_eq!(status, 200, "{edited}");
    assert_eq!(
        (&edited["state"], &edited["edits"]),
        (&json!("edited"), &json!(1))
    );
    assert_eq!(item(&edited, reference)["value"], other);
    let (status, technical) = get(&server, &format!("{device}/technical")).await;
    assert_eq!(status, 200);
    assert!(
        technical["variants"][0]["requests"]
            .as_array()
            .unwrap()
            .len()
            > 10
    );

    // Comparing reads the device, and that is allowed.
    let (status, started) = post(&server, &format!("{device}/compare"), json!({})).await;
    assert_eq!(status, 202, "{started}");
    let job = finished(&server, started["job"].as_u64().unwrap()).await;
    assert_eq!(job["status"], "succeeded", "{job}");
    assert!(job["result"]["compared"].as_u64().unwrap() > 0);
    let (_, state) = get(&server, "/api/job").await;
    assert!(!state["log"].as_array().unwrap().iter().any(|line| {
        let text = line["text"].as_str().unwrap();
        line["level"] == "request"
            && (text.starts_with("MEM_WRITE")
                || text.starts_with("PROP_WRITE")
                || text == "RESTART")
    }));

    // Downloading is not: refused with the reason, confirmed or not, and nothing is sent.
    let (status, refusal) = post(
        &server,
        &format!("{device}/download"),
        json!({ "confirm": true }),
    )
    .await;
    assert_eq!(status, 422);
    assert_eq!(refusal["error"], blocked);
    let (_, jobs) = get(&server, "/api/jobs").await;
    assert_eq!(jobs["jobs"].as_array().unwrap().len(), 1);

    // A dialog plug-in is another matter: nothing to edit, nothing to compare.
    let (status, _) = put(
        &server,
        "/api/devices/P-0001-0_DI-4/parameters/P-1_R-1",
        json!({ "value": "1" }),
    )
    .await;
    assert_eq!(status, 422);
    let (status, _) = post(&server, "/api/devices/P-0001-0_DI-4/compare", json!({})).await;
    assert_eq!(status, 422);
}

#[tokio::test(flavor = "multi_thread")]
async fn routing_without_a_named_interface_is_described_as_it_is_now() {
    // Nothing is sent here: the interface is only chosen and described. Which one it is
    // depends on the machine the test runs on, and so does whether there is one at all.
    let server = serve_with(|_| Bus::AutoRouting(tool())).await;
    let (status, overview) = get(&server, "/api/project").await;
    assert_eq!(status, 200);
    let bus = &overview["bus"];
    assert_eq!(bus["mode"], "routing");
    if bus["interface"].is_null() {
        assert_eq!(bus["text"], "routing: no network");
        assert!(
            bus["warning"]
                .as_str()
                .unwrap()
                .contains("not on a network")
        );
    } else {
        assert!(bus["interface"]["address"].is_string(), "{bus}");
        assert!(bus["text"].as_str().unwrap().ends_with("as 0.0.2"), "{bus}");
        assert_eq!(bus["warning"].is_string(), bus["wireless"] == true);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_page_is_told_which_interface_routing_uses() {
    // Nothing is sent here: the bus access is only described.
    let routing = |name: &str, kind: Kind| {
        Bus::Access(BusAccess::Routing {
            interface: "192.0.2.7".parse().unwrap(),
            name: name.into(),
            kind,
            address: tool(),
            group: None,
        })
    };
    let server = serve_with(|_| routing("wlan0", Kind::Wireless)).await;
    let (_, overview) = get(&server, "/api/project").await;
    let bus = &overview["bus"];
    assert_eq!(bus["mode"], "routing");
    assert_eq!(bus["text"], "routing on wlan0 (wireless) as 0.0.2");
    assert_eq!(
        bus["interface"],
        json!({ "name": "wlan0", "address": "192.0.2.7", "kind": "wireless" })
    );
    assert_eq!(bus["wireless"], true);
    let warning = bus["warning"].as_str().unwrap();
    assert!(
        warning.contains("Wi-Fi") && warning.contains("wired connection"),
        "{warning}"
    );

    let server = serve_with(|_| routing("enp3s0", Kind::Wired)).await;
    let (_, overview) = get(&server, "/api/project").await;
    let bus = &overview["bus"];
    assert_eq!(bus["text"], "routing on enp3s0 (wired) as 0.0.2");
    assert_eq!(
        (&bus["wireless"], &bus["warning"]),
        (&json!(false), &Value::Null)
    );

    let server = serve_with(|_| routing("", Kind::Unknown)).await;
    let (_, overview) = get(&server, "/api/project").await;
    assert_eq!(
        overview["bus"]["text"],
        "routing on 192.0.2.7 (kind unknown) as 0.0.2"
    );
    assert_eq!(overview["bus"]["wireless"], false);
}

// ------------------------------------------------------------------------------------------
// Programming several devices one after another
// ------------------------------------------------------------------------------------------

const ACTUATOR: &str = "P-0001-0_DI-2";

/// Wait until the queue is no longer at work and return it.
async fn queue_done(server: &str) -> Value {
    tokio::time::timeout(WAIT, async {
        loop {
            let (_, state) = get(server, "/api/program").await;
            if state["queue"]["active"] == false {
                return state["queue"].clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the queue ends")
}

fn statuses(queue: &Value) -> Vec<&str> {
    queue["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["status"].as_str().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn devices_are_programmed_one_after_another() {
    let server = serve().await;
    let (_, none) = get(&server, "/api/program").await;
    assert_eq!(none["queue"], Value::Null);
    // Edits on two devices; a third goes along as the project has it.
    put(
        &server,
        &format!("/api/devices/{DIMMER}/parameters/P-1_R-1"),
        json!({ "value": "40" }),
    )
    .await;
    let order = [SENSOR, DIMMER, ACTUATOR];
    let (status, started) = post(
        &server,
        "/api/program",
        json!({ "devices": order, "confirm": true }),
    )
    .await;
    assert_eq!(status, 202, "{started}");
    assert_eq!(started["queue"]["active"], true);
    assert_eq!(
        statuses(&started["queue"]),
        ["waiting", "waiting", "waiting"]
    );
    // While it is at work nothing else starts.
    let (status, _) = post(
        &server,
        &format!("/api/devices/{ACTUATOR}/compare"),
        json!({}),
    )
    .await;
    assert_eq!(status, 409);
    let (status, _) = post(
        &server,
        "/api/program",
        json!({ "devices": [DIMMER], "confirm": true }),
    )
    .await;
    assert_eq!(status, 409);

    let queue = queue_done(&server).await;
    assert_eq!(statuses(&queue), ["done", "done", "done"]);
    // Ordinary download jobs, in the order asked for, each finished before the next began.
    let (_, jobs) = get(&server, "/api/jobs").await;
    let mut jobs = jobs["jobs"].as_array().unwrap().clone();
    jobs.reverse();
    let devices: Vec<&str> = jobs
        .iter()
        .map(|job| job["device"].as_str().unwrap())
        .collect();
    assert_eq!(devices, order);
    for (job, entry) in jobs.iter().zip(queue["entries"].as_array().unwrap()) {
        assert_eq!(
            (&job["kind"], &job["status"]),
            (&json!("download"), &json!("succeeded"))
        );
        assert_eq!(job["id"], entry["job"]);
    }
    for pair in jobs.windows(2) {
        assert!(pair[0]["finished_ms"].as_u64() <= pair[1]["started_ms"].as_u64());
    }
    // Every one of them is downloaded now, and nothing is pending.
    let (_, overview) = get(&server, "/api/project").await;
    for row in overview["devices"].as_array().unwrap() {
        if order.contains(&row["id"].as_str().unwrap()) {
            assert_eq!(row["state"], "downloaded", "{row}");
        }
    }
    assert_eq!(overview["queue"]["active"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_that_fails_stops_the_queue() {
    // The second device is gone for good after its first frames.
    let server = serve_with(|project| {
        let bus = Bus::simulate(project, tool());
        if let Bus::Simulated(simulated, _) = &bus {
            simulated.with("1.1.9".parse().unwrap(), |endpoint| {
                endpoint.faults.silent_after = Some(12);
            });
        }
        bus
    })
    .await;
    let (status, _) = post(
        &server,
        "/api/program",
        json!({ "devices": [SENSOR, DIMMER, ACTUATOR], "confirm": true }),
    )
    .await;
    assert_eq!(status, 202);
    let queue = queue_done(&server).await;
    // The first went through, the second failed, the third was never touched.
    assert_eq!(statuses(&queue), ["done", "failed", "pending"]);
    assert_eq!(queue["entries"][2]["job"], Value::Null);
    let (_, jobs) = get(&server, "/api/jobs").await;
    assert_eq!(jobs["jobs"].as_array().unwrap().len(), 2);
    let failed = queue["entries"][1]["job"].as_u64().unwrap();
    let (_, job) = get(&server, &format!("/api/jobs/{failed}")).await;
    assert_eq!(job["job"]["status"], "failed");
    assert!(job["job"]["written"].is_string());
    let (_, overview) = get(&server, "/api/project").await;
    let rows = overview["devices"].as_array().unwrap();
    let row = |id: &str| rows.iter().find(|row| row["id"] == id).unwrap();
    assert_eq!(row(DIMMER)["attention"], "Programming failed");
    assert_ne!(row(ACTUATOR)["state"], "downloaded");
    // A new run is possible once this one is over.
    let (status, _) = post(
        &server,
        "/api/program",
        json!({ "devices": [ACTUATOR], "confirm": true }),
    )
    .await;
    assert_eq!(status, 202);
    assert_eq!(statuses(&queue_done(&server).await), ["done"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_stops_the_download_and_the_queue() {
    // The first device takes no frame: its download waits for acknowledgements.
    let server = serve_with(|project| {
        let bus = Bus::simulate(project, tool());
        if let Bus::Simulated(simulated, _) = &bus {
            simulated.with("1.1.9".parse().unwrap(), |endpoint| {
                endpoint.faults.lose_requests = u32::MAX;
            });
        }
        bus
    })
    .await;
    let (status, _) = post(
        &server,
        "/api/program",
        json!({ "devices": [DIMMER, SENSOR], "confirm": true }),
    )
    .await;
    assert_eq!(status, 202);
    // Wait for the first download to be under way, then cancel.
    tokio::time::timeout(WAIT, async {
        loop {
            let (_, state) = get(&server, "/api/program").await;
            if state["queue"]["entries"][0]["status"] == "running" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let (_, cancelled) = post(&server, "/api/job/cancel", json!({})).await;
    assert_eq!(cancelled["cancelled"], true);
    let queue = queue_done(&server).await;
    assert_eq!(statuses(&queue), ["failed", "pending"]);
    let (_, jobs) = get(&server, "/api/jobs").await;
    let jobs = jobs["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["status"], "cancelled");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queue_with_a_device_that_cannot_be_programmed_is_refused() {
    let server = serve().await;
    let program = |devices: Value, confirm: Value| {
        let server = server.clone();
        async move {
            post(
                &server,
                "/api/program",
                json!({ "devices": devices, "confirm": confirm }),
            )
            .await
        }
    };
    // Not confirmed, nothing named, something unknown, something named twice.
    assert_eq!(program(json!([DIMMER]), json!(false)).await.0, 400);
    assert_eq!(program(json!([DIMMER]), json!("yes")).await.0, 400);
    assert_eq!(program(json!([]), json!(true)).await.0, 400);
    assert_eq!(program(json!("all"), json!(true)).await.0, 400);
    assert_eq!(program(json!([DIMMER, "nobody"]), json!(true)).await.0, 404);
    assert_eq!(program(json!([DIMMER, DIMMER]), json!(true)).await.0, 422);
    // A device that can be edited and compared but not downloaded, and one that cannot be
    // used at all: the whole request is refused, with the device named.
    for other in ["P-0001-0_DI-5", "P-0001-0_DI-4"] {
        let (status, refusal) = program(json!([DIMMER, other]), json!(true)).await;
        assert_eq!(status, 422, "{refusal}");
        assert!(
            refusal["error"]
                .as_str()
                .unwrap()
                .contains("cannot be programmed here"),
            "{refusal}"
        );
    }
    // From another site.
    let (status, _) = http(
        &server,
        "POST",
        "/api/program",
        &[("Origin", "http://evil.example")],
        &json!({ "devices": [DIMMER], "confirm": true }).to_string(),
    )
    .await;
    assert_eq!(status, 403);
    // Nothing of all that started anything.
    let (_, state) = get(&server, "/api/program").await;
    assert_eq!(state["queue"], Value::Null);
    let (_, jobs) = get(&server, "/api/jobs").await;
    assert_eq!(jobs["jobs"], json!([]));
    // Without a bus there is nothing to program on.
    let server = serve_with(|_| Bus::None).await;
    let (status, _) = post(
        &server,
        "/api/program",
        json!({ "devices": [DIMMER], "confirm": true }),
    )
    .await;
    assert_eq!(status, 409);
}

// ------------------------------------------------------------------------------------------
// Floor plans
// ------------------------------------------------------------------------------------------

/// One request with a body of bytes; returns the status, the content type and the body.
async fn http_bytes(server: &str, method: &str, path: &str, body: &[u8]) -> (u16, String, Vec<u8>) {
    let mut stream = TcpStream::connect(server).await.unwrap();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {server}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    // A server that refuses a body for its size may close before all of it is written.
    let _ = stream.write_all(body).await;
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response).await;
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a response");
    let head = String::from_utf8_lossy(&response[..split]).into_owned();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    let content_type = head
        .lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-type: ")
                .map(str::to_string)
        })
        .unwrap_or_default();
    (status, content_type, response[split + 4..].to_vec())
}

/// The smallest thing that passes for a PNG, a JPEG and a WebP image here: the first bytes
/// of each and some more.
fn images() -> [(&'static str, Vec<u8>); 3] {
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend([0u8; 40]);
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
    jpeg.extend([1u8; 40]);
    let mut webp = b"RIFF\x20\0\0\0WEBPVP8 ".to_vec();
    webp.extend([2u8; 40]);
    [
        ("image/png", png),
        ("image/jpeg", jpeg),
        ("image/webp", webp),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn floor_plans_are_kept_with_their_images_and_places() {
    let directory = scratch("plans");
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("demo.knxproj.knxide.json");
    let server = serve_keeping(&file).await;
    let (status, none) = get(&server, "/api/floor-plans").await;
    assert_eq!((status, &none["plans"]), (200, &json!([])));

    // One plan per kind of image that is taken.
    let mut ids = Vec::new();
    for (number, (media, bytes)) in images().into_iter().enumerate() {
        let name = ["Ground%20floor", "", "Attic"][number];
        let (status, _, body) = http_bytes(
            &server,
            "POST",
            &format!("/api/floor-plans?name={name}"),
            &bytes,
        )
        .await;
        assert_eq!(status, 201, "{media}");
        let plan: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(plan["name"], ["Ground floor", "Plan 2", "Attic"][number]);
        assert_eq!(plan["positions"], json!({}));
        let id = plan["id"].as_str().unwrap().to_string();
        // The image comes back as it went in, under its own kind.
        let (status, content_type, served) =
            http_bytes(&server, "GET", plan["image"].as_str().unwrap(), &[]).await;
        assert_eq!((status, content_type.as_str()), (200, media));
        assert_eq!(served, bytes);
        ids.push(id);
    }
    assert_eq!(ids, ["plan-1", "plan-2", "plan-3"]);

    // What is not one of those is refused, whatever it is called; so is what is too large.
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
    for bytes in [&svg[..], b"GIF89a....", b"", b"just text"] {
        let (status, _, body) =
            http_bytes(&server, "POST", "/api/floor-plans?name=x.png", bytes).await;
        assert_eq!(status, 415, "{}", String::from_utf8_lossy(&body));
    }
    let mut large = images()[0].1.clone();
    large.resize(8 * 1024 * 1024 + 1, 0);
    let (status, _, _) = http_bytes(&server, "POST", "/api/floor-plans", &large).await;
    assert_eq!(status, 413);
    let (status, _, _) = http_bytes(&server, "PUT", "/api/floor-plans/plan-1/image", svg).await;
    assert_eq!(status, 415);
    let (_, list) = get(&server, "/api/floor-plans").await;
    assert_eq!(list["plans"].as_array().unwrap().len(), 3);

    // Places: percentages of the image, by device.
    let path = format!("/api/floor-plans/{}", ids[0]);
    let places = json!({ DIMMER: { "x": 42.5, "y": 61.25 }, SENSOR: { "x": 0, "y": 100 } });
    let (status, plan) = put(&server, &path, json!({ "positions": places })).await;
    assert_eq!(status, 200, "{plan}");
    assert_eq!(
        plan["positions"],
        json!({ DIMMER: { "x": 42.5, "y": 61.25 }, SENSOR: { "x": 0.0, "y": 100.0 } })
    );
    assert_eq!(plan["name"], "Ground floor");
    for bad in [
        json!({ "positions": { DIMMER: { "x": 101, "y": 5 } } }),
        json!({ "positions": { DIMMER: { "x": -1, "y": 5 } } }),
        json!({ "positions": { DIMMER: { "x": "left" } } }),
        json!({ "positions": { "nobody": { "x": 1, "y": 1 } } }),
        json!({ "positions": [1, 2] }),
        json!({ "name": 7 }),
        json!({ "name": "n".repeat(61) }),
    ] {
        let (status, refusal) = put(&server, &path, bad.clone()).await;
        assert_eq!(status, 422, "{bad}: {refusal}");
    }
    let (status, _) = put(&server, "/api/floor-plans/plan-9", json!({ "name": "x" })).await;
    assert_eq!(status, 404);
    // A new name leaves the places, and the other way round.
    let (_, plan) = put(&server, &path, json!({ "name": "Downstairs" })).await;
    assert_eq!(plan["name"], "Downstairs");
    assert_eq!(plan["positions"][DIMMER]["x"], 42.5);
    // Another image for a plan: served under a new address, the places stay.
    let (status, _, body) =
        http_bytes(&server, "PUT", &format!("{path}/image"), &images()[1].1).await;
    assert_eq!(status, 200);
    let replaced: Value = serde_json::from_slice(&body).unwrap();
    assert_ne!(replaced["image"], plan["image"]);
    assert_eq!(replaced["positions"], plan["positions"]);
    let (_, content_type, served) =
        http_bytes(&server, "GET", replaced["image"].as_str().unwrap(), &[]).await;
    assert_eq!(content_type, "image/jpeg");
    assert_eq!(served, images()[1].1);
    // Not from another site.
    let (status, _) = http(
        &server,
        "PUT",
        &path,
        &[("Origin", "http://evil.example")],
        r#"{"name":"x"}"#,
    )
    .await;
    assert_eq!(status, 403);
    let (status, _) = http(
        &server,
        "DELETE",
        &path,
        &[("Origin", "http://evil.example")],
        "",
    )
    .await;
    assert_eq!(status, 403);

    // Beside the state file: the plans in it, their images in a directory of their own, and
    // nothing left over from the image that was replaced.
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(kept["floor_plans"].as_array().unwrap().len(), 3);
    assert_eq!(
        kept["floor_plans"][0]["positions"][DIMMER],
        json!({ "x": 4250, "y": 6125 })
    );
    let mut files: Vec<String> =
        std::fs::read_dir(directory.join("demo.knxproj.knxide.json.plans"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
    files.sort();
    assert_eq!(files, ["plan-1-2.jpg", "plan-2-1.jpg", "plan-3-1.webp"]);

    // Another start of the program finds plans, places and images again.
    let server = serve_keeping(&file).await;
    let (_, list) = get(&server, "/api/floor-plans").await;
    let plans = list["plans"].as_array().unwrap();
    assert_eq!(plans.len(), 3);
    assert_eq!(plans[0], replaced);
    let (status, _, served) =
        http_bytes(&server, "GET", plans[2]["image"].as_str().unwrap(), &[]).await;
    assert_eq!(status, 200);
    assert_eq!(served, images()[2].1);

    // Removed: gone from the list, the state file and the directory. Its number is free.
    let (status, _) = delete(&server, "/api/floor-plans/plan-2").await;
    assert_eq!(status, 200);
    let (status, _) = delete(&server, "/api/floor-plans/plan-2").await;
    assert_eq!(status, 404);
    let (status, _, _) = http_bytes(&server, "GET", "/api/floor-plans/plan-2/image", &[]).await;
    assert_eq!(status, 404);
    assert!(
        !directory
            .join("demo.knxproj.knxide.json.plans/plan-2-1.jpg")
            .exists()
    );
    let (_, list) = get(&server, "/api/floor-plans").await;
    assert_eq!(list["plans"].as_array().unwrap().len(), 2);
    let (_, _, body) = http_bytes(&server, "POST", "/api/floor-plans", &images()[0].1).await;
    let plan: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(plan["id"], "plan-2");
    std::fs::remove_dir_all(&directory).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_device_payload_says_what_a_device_is_and_talks_to() {
    let server = serve().await;
    let (_, overview) = get(&server, "/api/project").await;
    let rows = overview["devices"].as_array().unwrap();
    let kinds: Vec<(&str, &str)> = rows
        .iter()
        .map(|row| (row["kind"].as_str().unwrap(), row["type"].as_str().unwrap()))
        .collect();
    assert_eq!(kinds[0], ("dimmer", "Dimmer"));
    assert_eq!(kinds[1], ("switch", "Switch actuator"));
    assert_eq!(kinds[2], ("sensor", "Sensor"));
    assert!(rows.iter().all(|row| row["attention"].is_null()));
    // Nothing in the made-up project shares a group address, until two devices are linked to
    // the same one.
    let (_, dimmer) = get(&server, &format!("/api/devices/{DIMMER}")).await;
    assert_eq!(dimmer["talks_to"], json!([]));
    put(
        &server,
        &format!("/api/devices/{DIMMER}/objects/O-0_R-1"),
        json!({ "links": ["1/2/2", "1/0/1"] }),
    )
    .await;
    let (_, dimmer) = get(&server, &format!("/api/devices/{DIMMER}")).await;
    assert_eq!(dimmer["talks_to"], json!([SENSOR]));
    let (_, sensor) = get(&server, &format!("/api/devices/{SENSOR}")).await;
    assert_eq!(sensor["talks_to"], json!([DIMMER]));
    // A comparison that finds the device different asks for attention in the list.
    let (_, started) = post(
        &server,
        &format!("/api/devices/{DIMMER}/compare"),
        json!({}),
    )
    .await;
    finished(&server, started["job"].as_u64().unwrap()).await;
    let (_, overview) = get(&server, "/api/project").await;
    assert_eq!(overview["devices"][0]["attention"], "Differs from device");
}

// ---------------------------------------------------------------- the baseline

/// A server like [`serve_keeping`], whose dimmer (1.1.9) has `faults`.
async fn serve_keeping_faulty(state: &std::path::Path, faults: Faults) -> String {
    let project = project();
    let bus = Bus::simulate(&project, tool());
    if let Bus::Simulated(simulated, _) = &bus {
        simulated.with("1.1.9".parse().unwrap(), |endpoint| {
            endpoint.faults = faults
        });
    }
    let settings = Settings {
        session: fast(),
        limits: limits(),
        download: DownloadLimits::none(),
        state: Some(state.to_path_buf()),
    };
    let programmer = Programmer::open(project, "demo.knxproj".into(), bus, settings).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app(programmer)).await.unwrap() });
    address.to_string()
}

/// Program `devices` through the queue and wait for it to end.
async fn program(server: &str, devices: &[&str]) -> Value {
    let (status, started) = post(
        server,
        "/api/program",
        json!({ "devices": devices, "confirm": true }),
    )
    .await;
    assert_eq!(status, 202, "{started}");
    queue_done(server).await
}

fn kept(file: &std::path::Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap()
}

/// The device's row in the overview.
async fn row(server: &str, id: &str) -> Value {
    let (_, overview) = get(server, "/api/project").await;
    overview["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap()
        .clone()
}

/// The parameter changes of an editor payload as `from -> to`.
fn pending(editor: &Value) -> Vec<String> {
    editor["changes"]["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| {
            format!(
                "{} -> {}",
                change["from"].as_str().unwrap(),
                change["to"].as_str().unwrap()
            )
        })
        .collect()
}

/// The owner's sequence: a value is changed and programmed, then set back to the project's
/// value. That is a change like any other: it is measured against what the device was
/// programmed with, not against the project file.
#[tokio::test(flavor = "multi_thread")]
async fn changed_is_measured_against_what_was_programmed() {
    let directory = scratch("baseline");
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("demo.knxproj.knxide.json");
    let device = format!("/api/devices/{DIMMER}");
    let level = format!("{device}/parameters/P-1_R-1");
    let server = serve_keeping(&file).await;

    // Never programmed here: the project is the baseline.
    let (_, editor) = get(&server, &device).await;
    let project_value = item(&editor, "P-1_R-1")["value"].clone();
    assert_eq!(project_value, "6");
    let (_, editor) = put(&server, &level, json!({ "value": "7" })).await;
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("edited"), &json!(1))
    );
    assert_eq!(pending(&editor), ["6 % -> 7 %"]);

    // Programmed: nothing on the device is marked.
    let queue = program(&server, &[DIMMER]).await;
    assert_eq!(statuses(&queue), ["done"]);
    let (_, editor) = get(&server, &device).await;
    assert_eq!(
        (&editor["state"], &editor["edits"], &editor["pending_note"]),
        (&json!("downloaded"), &json!(0), &Value::Null)
    );
    assert_eq!(pending(&editor), Vec::<String>::new());
    assert_eq!(editor["changes"]["links"], json!([]));
    assert_ne!(item(&editor, "P-1_R-1")["standing"], "edited");
    assert_eq!(row(&server, DIMMER).await["state"], "downloaded");
    // What differs from the project file is still told, on its own.
    let apart = &editor["from_project"]["parameters"];
    assert_eq!(apart.as_array().unwrap().len(), 1, "{apart}");
    assert_eq!(
        (&apart[0]["from"], &apart[0]["to"]),
        (&json!("6 %"), &json!("7 %"))
    );
    let record = &kept(&file)["devices"][DIMMER];
    assert_eq!(record["parameters"]["P-1_R-1"], "7");
    assert_eq!(
        record["downloaded"]["configuration"]["parameters"]["P-1_R-1"],
        "7"
    );

    // Set back to the project's value: one change, from what is in the device.
    let (status, editor) = put(&server, &level, json!({ "value": "6" })).await;
    assert_eq!(status, 200, "{editor}");
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("edited"), &json!(1))
    );
    assert_eq!(pending(&editor), ["7 % -> 6 %"]);
    let shown = item(&editor, "P-1_R-1");
    assert_eq!(
        (
            &shown["standing"],
            &shown["value"],
            &shown["baseline_value"]
        ),
        (&json!("edited"), &json!("6"), &json!("7"))
    );
    assert_eq!(editor["from_project"]["parameters"], json!([]));
    assert_eq!(row(&server, DIMMER).await["state"], "edited");
    let record = &kept(&file)["devices"][DIMMER];
    assert_eq!(record["parameters"], Value::Null);
    assert_eq!(
        record["downloaded"]["configuration"]["parameters"]["P-1_R-1"],
        "7"
    );

    // The same after a restart of the program.
    let server = serve_keeping(&file).await;
    let (_, editor) = get(&server, &device).await;
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("edited"), &json!(1))
    );
    assert_eq!(pending(&editor), ["7 % -> 6 %"]);

    // Programmed again: nothing is marked, and the file says what the device holds.
    let queue = program(&server, &[DIMMER]).await;
    assert_eq!(statuses(&queue), ["done"]);
    let (_, editor) = get(&server, &device).await;
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("downloaded"), &json!(0))
    );
    assert_eq!(pending(&editor), Vec::<String>::new());
    assert_ne!(item(&editor, "P-1_R-1")["standing"], "edited");
    let record = &kept(&file)["devices"][DIMMER];
    assert_eq!(record["parameters"], Value::Null);
    // The project's own configuration: nothing in it is different.
    assert_eq!(
        record["downloaded"]["configuration"],
        json!({ "parameters": {}, "links": {} })
    );
    assert!(record["downloaded"]["digest"].is_string());
    assert!(record["downloaded"]["time_ms"].is_u64());
    assert_eq!(record["interrupted"], Value::Null);

    // The queue takes what it took before: a device with nothing to program, too.
    let queue = program(&server, &[DIMMER, SENSOR]).await;
    assert_eq!(statuses(&queue), ["done", "done"]);
    std::fs::remove_dir_all(&directory).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_and_discard_return_to_what_was_programmed() {
    let server = serve().await;
    let device = format!("/api/devices/{DIMMER}");
    let level = format!("{device}/parameters/P-1_R-1");
    let object = format!("{device}/objects/O-0_R-1");
    let (_, editor) = get(&server, &device).await;
    let project_links = links(&editor);

    // Never programmed here: undo and discard go to the project.
    put(&server, &level, json!({ "value": "7" })).await;
    let (_, editor) = delete(&server, &level).await;
    assert_eq!(item(&editor, "P-1_R-1")["value"], "6");
    assert_eq!(editor["state"], "project");

    // Programmed with another value and another link.
    put(&server, &level, json!({ "value": "7" })).await;
    let (_, editor) = put(&server, &object, json!({ "links": ["1/1/1", "1/2/2"] })).await;
    let programmed_links = links(&editor);
    assert_ne!(programmed_links, project_links);
    program(&server, &[DIMMER]).await;

    // Undo: to what is in the device, from another value and from the project's.
    for value in ["9", "6"] {
        let (_, editor) = put(&server, &level, json!({ "value": value })).await;
        assert_eq!(editor["state"], "edited", "{value}");
        let (status, editor) = delete(&server, &level).await;
        assert_eq!(status, 200, "{editor}");
        assert_eq!(item(&editor, "P-1_R-1")["value"], "7", "{value}");
        assert_eq!(
            (&editor["state"], &editor["edits"]),
            (&json!("downloaded"), &json!(0))
        );
    }

    // Discard: the same for everything at once.
    put(&server, &level, json!({ "value": "6" })).await;
    let (_, editor) = put(&server, &object, json!({ "links": ["1/2/2"] })).await;
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("edited"), &json!(2))
    );
    let (status, editor) = delete(&server, &format!("{device}/edits")).await;
    assert_eq!(status, 200, "{editor}");
    assert_eq!(item(&editor, "P-1_R-1")["value"], "7");
    assert_eq!(links(&editor), programmed_links);
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("downloaded"), &json!(0))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_that_stopped_leaves_the_device_pending() {
    let directory = scratch("stopped");
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("demo.knxproj.knxide.json");
    let device = format!("/api/devices/{DIMMER}");
    // After 20 numbered frames the device is gone for good: writing had begun.
    let server = serve_keeping_faulty(
        &file,
        Faults {
            silent_after: Some(20),
            ..Faults::default()
        },
    )
    .await;
    let queue = program(&server, &[DIMMER]).await;
    assert_eq!(statuses(&queue), ["failed"]);
    let (_, editor) = get(&server, &device).await;
    assert!(editor["last"]["progress"]["writes"].as_u64().unwrap() > 0);
    // Nothing differs from the project, and still the device waits for a download.
    let stopped = "Programming stopped half-way. Program it again.";
    assert_eq!(
        (&editor["state"], &editor["edits"], &editor["pending_note"]),
        (&json!("edited"), &json!(0), &json!(stopped))
    );
    assert_eq!(pending(&editor), Vec::<String>::new());
    assert_eq!(kept(&file)["devices"][DIMMER]["interrupted"], true);

    // Another start of the program, with the device back: it still waits, whatever is
    // edited meanwhile.
    let server = serve_keeping(&file).await;
    let waiting = row(&server, DIMMER).await;
    assert_eq!(
        (&waiting["state"], &waiting["pending_note"]),
        (&json!("edited"), &json!(stopped))
    );
    let level = format!("{device}/parameters/P-1_R-1");
    put(&server, &level, json!({ "value": "7" })).await;
    let (_, editor) = delete(&server, &format!("{device}/edits")).await;
    assert_eq!(
        (&editor["state"], &editor["edits"], &editor["pending_note"]),
        (&json!("edited"), &json!(0), &json!(stopped))
    );
    assert_eq!(kept(&file)["devices"][DIMMER]["interrupted"], true);

    // Until a download goes through.
    let queue = program(&server, &[DIMMER]).await;
    assert_eq!(statuses(&queue), ["done"]);
    let done = row(&server, DIMMER).await;
    assert_eq!(
        (&done["state"], &done["pending_note"]),
        (&json!("downloaded"), &Value::Null)
    );
    assert_eq!(kept(&file)["devices"][DIMMER]["interrupted"], Value::Null);
    let server = serve_keeping(&file).await;
    assert_eq!(row(&server, DIMMER).await["state"], "downloaded");
    std::fs::remove_dir_all(&directory).unwrap();
}

/// State files written before configurations were kept name a digest only.
#[tokio::test(flavor = "multi_thread")]
async fn a_record_without_configuration_is_understood() {
    let directory = scratch("old-record");
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("demo.knxproj.knxide.json");
    let device = format!("/api/devices/{DIMMER}");
    let level = format!("{device}/parameters/P-1_R-1");

    // The digest of the configuration with the value 7, from a download of it.
    let server = serve_keeping(&file).await;
    put(&server, &level, json!({ "value": "7" })).await;
    program(&server, &[DIMMER]).await;
    let digest = kept(&file)["devices"][DIMMER]["downloaded"]["digest"].clone();
    let old = |value: &str| {
        let text = json!({
            "version": 1,
            "project": "P-0001",
            "devices": { DIMMER: {
                "parameters": { "P-1_R-1": value },
                "downloaded": { "time_ms": 1_700_000_000_000_u64, "digest": digest },
            } },
        });
        std::fs::write(&file, serde_json::to_string_pretty(&text).unwrap()).unwrap();
    };

    // The configuration here has that digest: it is what was programmed.
    old("7");
    let server = serve_keeping(&file).await;
    let (_, editor) = get(&server, &device).await;
    assert_eq!(
        (&editor["state"], &editor["edits"], &editor["pending_note"]),
        (&json!("downloaded"), &json!(0), &Value::Null)
    );
    assert_eq!(editor["downloaded_ms"], 1_700_000_000_000_u64);
    let (_, editor) = put(&server, &level, json!({ "value": "6" })).await;
    assert_eq!(pending(&editor), ["7 % -> 6 %"]);
    assert_eq!(
        kept(&file)["devices"][DIMMER]["downloaded"]["configuration"]["parameters"]["P-1_R-1"],
        "7"
    );

    // It has another: the device waits, and which items differ is not known.
    old("8");
    let before = std::fs::read_to_string(&file).unwrap();
    let server = serve_keeping(&file).await;
    let differs = "Differs from what was last programmed.";
    let (_, editor) = get(&server, &device).await;
    assert_eq!(
        (&editor["state"], &editor["edits"], &editor["pending_note"]),
        (&json!("edited"), &json!(0), &json!(differs))
    );
    assert_eq!(pending(&editor), Vec::<String>::new());
    assert_eq!(item(&editor, "P-1_R-1")["value"], "8");
    assert_eq!(row(&server, DIMMER).await["state"], "edited");
    // Looking at it changes nothing in the file.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    // The next download writes the whole record.
    let queue = program(&server, &[DIMMER]).await;
    assert_eq!(statuses(&queue), ["done"]);
    let (_, editor) = get(&server, &device).await;
    assert_eq!(
        (&editor["state"], &editor["edits"], &editor["pending_note"]),
        (&json!("downloaded"), &json!(0), &Value::Null)
    );
    let record = &kept(&file)["devices"][DIMMER]["downloaded"];
    assert_eq!(record["configuration"]["parameters"]["P-1_R-1"], "8");
    assert_ne!(record["digest"], digest);
    let (_, editor) = put(&server, &level, json!({ "value": "7" })).await;
    assert_eq!(pending(&editor), ["8 % -> 7 %"]);
    std::fs::remove_dir_all(&directory).unwrap();
}

/// A comparison that read everything and found no difference tells what the device holds.
#[tokio::test(flavor = "multi_thread")]
async fn a_comparison_without_difference_sets_the_baseline() {
    let device = format!("/api/devices/{DIMMER}");
    let level = format!("{device}/parameters/P-1_R-1");
    let compare = |server: String| async move {
        let (status, started) = post(
            &server,
            &format!("/api/devices/{DIMMER}/compare"),
            json!({}),
        )
        .await;
        assert_eq!(status, 202, "{started}");
        finished(&server, started["job"].as_u64().unwrap()).await
    };
    // The device was programmed with the value 7 by something else: another server on the
    // same simulated bus.
    let mut shared = None;
    let other = serve_with(|project| {
        let bus = Bus::simulate(project, tool());
        if let Bus::Simulated(simulated, _) = &bus {
            shared = Some(simulated.clone());
        }
        bus
    })
    .await;
    put(&other, &level, json!({ "value": "7" })).await;
    program(&other, &[DIMMER]).await;
    let bus = shared.unwrap();
    let server = serve_with(move |_| Bus::Simulated(bus, tool())).await;

    // As the project has it, the configuration here is not the device's: nothing is learnt.
    let job = compare(server.clone()).await;
    assert!(job["result"]["differing"].as_u64().unwrap() > 0, "{job}");
    let (_, editor) = get(&server, &device).await;
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("project"), &json!(0))
    );
    // With the value 7 it is. The edit is no change any more: the device holds it.
    let (_, editor) = put(&server, &level, json!({ "value": "7" })).await;
    assert_eq!(
        (&editor["state"], &editor["edits"]),
        (&json!("edited"), &json!(1))
    );
    let job = compare(server.clone()).await;
    assert_eq!(job["result"]["differing"], 0, "{job}");
    let (_, editor) = get(&server, &device).await;
    assert_eq!(
        (&editor["state"], &editor["edits"], &editor["downloaded_ms"]),
        (&json!("downloaded"), &json!(0), &Value::Null)
    );
    assert_eq!(pending(&editor), Vec::<String>::new());
    let (_, editor) = put(&server, &level, json!({ "value": "6" })).await;
    assert_eq!(pending(&editor), ["7 % -> 6 %"]);
}
