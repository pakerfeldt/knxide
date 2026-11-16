//! The list of projects end to end: bringing a project file in, opening it, copying,
//! renaming and removing, and writing a project out again with its edits in it. The project
//! is the test kit's made-up one; no bus is involved.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use knxide_programmer::{BusConfig, Settings, Workspace, serve};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const DIMMER: &str = "P-0001-0_DI-1";

/// A fresh directory for this test's files.
fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        "knxide-projects-test-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

async fn start(workspace: Workspace) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, serve(workspace)).await.unwrap() });
    address.to_string()
}

async fn library(directory: &std::path::Path) -> (Workspace, String) {
    let workspace = Workspace::library(
        directory.to_path_buf(),
        BusConfig::None,
        Settings::default(),
        "en-US".into(),
    )
    .unwrap();
    let server = start(workspace.clone()).await;
    (workspace, server)
}

/// One HTTP request; returns the status and the body as it came.
async fn request(server: &str, method: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(server).await.unwrap();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {server}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    let head = String::from_utf8_lossy(&response[..split]).into_owned();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    let mut body = response[split + 4..].to_vec();
    // A body sent in chunks: put them together.
    if head.to_lowercase().contains("transfer-encoding: chunked") {
        let mut joined = Vec::new();
        let mut rest = &body[..];
        loop {
            let line = rest.windows(2).position(|w| w == b"\r\n").unwrap();
            let size =
                usize::from_str_radix(String::from_utf8_lossy(&rest[..line]).trim(), 16).unwrap();
            if size == 0 {
                break;
            }
            joined.extend_from_slice(&rest[line + 2..line + 2 + size]);
            rest = &rest[line + 2 + size + 2..];
        }
        body = joined;
    }
    (status, body)
}

async fn call(server: &str, method: &str, path: &str, body: Value) -> (u16, Value) {
    let text = if body.is_null() {
        String::new()
    } else {
        body.to_string()
    };
    let (status, body) = request(server, method, path, text.as_bytes()).await;
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

/// Bring the made-up project in under `name` and open it. Returns its identifier.
async fn bring_in(server: &str, name: &str, file: &[u8]) -> String {
    let (status, body) = request(server, "POST", &format!("/api/projects?name={name}"), file).await;
    let project: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    assert_eq!(status, 201, "{project}");
    let id = project["id"].as_str().unwrap().to_string();
    let (status, list) = call(
        server,
        "POST",
        &format!("/api/projects/{id}/open"),
        Value::Null,
    )
    .await;
    assert_eq!(status, 200, "{list}");
    assert_eq!(list["open"], json!(id));
    id
}

#[tokio::test(flavor = "multi_thread")]
async fn projects_are_listed_opened_copied_renamed_and_removed() {
    let directory = scratch("library");
    let (_, server) = library(&directory).await;

    let (status, list) = call(&server, "GET", "/api/projects", Value::Null).await;
    assert_eq!(status, 200);
    assert_eq!(
        list,
        json!({ "projects": [], "open": null, "library": true })
    );
    // Nothing is open: what works on a project says so.
    let (status, refusal) = call(&server, "GET", "/api/model", Value::Null).await;
    assert_eq!(status, 409, "{refusal}");

    // Not a project file: nothing is listed.
    let (status, _) = request(&server, "POST", "/api/projects", b"not an archive").await;
    assert_eq!(status, 422);

    let file = knxide_testkit::project::archive();
    let id = bring_in(&server, "House", &file).await;
    let (status, model) = call(&server, "GET", "/api/model", Value::Null).await;
    assert_eq!(status, 200);
    assert_eq!(model["devices"].as_array().unwrap().len(), 8);
    assert!(model["spaces"].is_array() && model["addresses"].is_array());
    assert!(model["devices"][0]["objects"].is_array());

    // The name is the one asked for; a second project of that name gets a number.
    let second = bring_in(&server, "House", &file).await;
    let (_, list) = call(&server, "GET", "/api/projects", Value::Null).await;
    let names: Vec<&str> = list["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|project| project["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["House", "House 2"]);

    // An edit in the first project goes with its copy.
    call(
        &server,
        "POST",
        &format!("/api/projects/{id}/open"),
        Value::Null,
    )
    .await;
    let parameter = format!("/api/devices/{DIMMER}/parameters/P-1_R-1");
    let (status, _) = call(&server, "PUT", &parameter, json!({ "value": "42" })).await;
    assert_eq!(status, 200);
    let (status, copy) = call(
        &server,
        "POST",
        &format!("/api/projects/{id}/copy"),
        json!({ "name": "Trial" }),
    )
    .await;
    assert_eq!(status, 201, "{copy}");
    let copy = copy["id"].as_str().unwrap().to_string();
    call(
        &server,
        "POST",
        &format!("/api/projects/{copy}/open"),
        Value::Null,
    )
    .await;
    let (_, editor) = call(
        &server,
        "GET",
        &format!("/api/devices/{DIMMER}"),
        Value::Null,
    )
    .await;
    assert_eq!(editor["edits"], 1, "the copy has the edit");

    let (status, renamed) = call(
        &server,
        "PUT",
        &format!("/api/projects/{copy}"),
        json!({ "name": "House", "summary": { "things": 3 } }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(renamed["name"], "House 3", "names stay apart");
    assert_eq!(renamed["summary"]["things"], 3);
    let (status, _) = call(
        &server,
        "PUT",
        &format!("/api/projects/{copy}"),
        json!({ "name": " " }),
    )
    .await;
    assert_eq!(status, 422);

    // Removing a project of the library takes its files; the open one is closed first.
    let kept = directory.join(&copy);
    assert!(kept.join("project.knxproj").is_file());
    let (status, list) = call(
        &server,
        "DELETE",
        &format!("/api/projects/{copy}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(list["open"], Value::Null);
    assert!(!kept.exists());
    assert_eq!(list["projects"].as_array().unwrap().len(), 2);
    let (status, _) = call(&server, "DELETE", "/api/projects/nobody", Value::Null).await;
    assert_eq!(status, 404);

    // The list is kept: another server on the same directory has it.
    let (_, again) = library(&directory).await;
    let (_, list) = call(&again, "GET", "/api/projects", Value::Null).await;
    assert_eq!(list["projects"].as_array().unwrap().len(), 2);
    assert_eq!(list["open"], Value::Null);
    let _ = (second, std::fs::remove_dir_all(&directory));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_named_at_start_stays_where_it_is() {
    let directory = scratch("library");
    let elsewhere = scratch("elsewhere");
    let file = elsewhere.join("demo.knxproj");
    std::fs::write(&file, knxide_testkit::project::archive()).unwrap();

    let (workspace, server) = library(&directory).await;
    let id = workspace.register(&file, None).unwrap();
    assert_eq!(workspace.register(&file, None).unwrap(), id, "listed once");
    workspace.open_listed(&id).await.unwrap();
    let (_, list) = call(&server, "GET", "/api/projects", Value::Null).await;
    assert_eq!(list["open"], json!(id));
    assert_eq!(list["projects"][0]["name"], "Demo house");
    assert_eq!(list["projects"][0]["owned"], false);

    // Its edits are kept beside it, and removing it from the list leaves the file.
    let parameter = format!("/api/devices/{DIMMER}/parameters/P-1_R-1");
    call(&server, "PUT", &parameter, json!({ "value": "42" })).await;
    assert!(elsewhere.join("demo.knxproj.knxide.json").is_file());
    let (status, _) = call(
        &server,
        "DELETE",
        &format!("/api/projects/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, 200);
    assert!(file.is_file());
    let _ = (
        std::fs::remove_dir_all(&directory),
        std::fs::remove_dir_all(&elsewhere),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_project_is_written_out_with_its_edits() {
    let directory = scratch("library");
    let (_, server) = library(&directory).await;
    let file = knxide_testkit::project::archive();
    let id = bring_in(&server, "House", &file).await;
    let export = format!("/api/projects/{id}/export");

    // Nothing changed: the file that went in comes out.
    let (status, same) = request(&server, "GET", &export, b"").await;
    assert_eq!(status, 200);
    assert_eq!(same, file);

    // A parameter, the links of an object, a name for an address, and two new addresses: one
    // in a middle group the project has, one where it has no group at all.
    let device = format!("/api/devices/{DIMMER}");
    let (status, _) = call(
        &server,
        "PUT",
        &format!("{device}/parameters/P-1_R-1"),
        json!({ "value": "42" }),
    )
    .await;
    assert_eq!(status, 200);
    for (address, name) in [("1/1/9", "Hall night light"), ("3/0/7", "Shed & <yard>")] {
        let (status, made) = call(
            &server,
            "POST",
            "/api/group-addresses",
            json!({ "address": address, "name": name, "datapoint_type": "DPST-1-1" }),
        )
        .await;
        assert_eq!(status, 201, "{made}");
    }
    let (status, _) = call(
        &server,
        "PUT",
        "/api/group-addresses/2305",
        json!({ "name": "Hall ceiling" }),
    )
    .await;
    assert_eq!(status, 200);
    let (status, editor) = call(
        &server,
        "PUT",
        &format!("{device}/objects/O-0_R-1"),
        json!({ "links": ["1/1/9", "3/0/7", "1/1/1"] }),
    )
    .await;
    assert_eq!(status, 200, "{editor}");
    let (_, edited) = call(&server, "GET", &device, Value::Null).await;

    let (status, exported) = request(&server, "GET", &export, b"").await;
    assert_eq!(status, 200);
    assert_ne!(exported, file);

    // Brought in again, the file holds as its own what were edits before.
    let again = bring_in(&server, "Again", &exported).await;
    let (_, reread) = call(&server, "GET", &device, Value::Null).await;
    assert_eq!(reread["edits"], 0);
    assert_eq!(reread["state"], "project");
    let links = |editor: &Value| -> Vec<String> {
        editor["objects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|object| {
                let addresses: Vec<&str> = object["links"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|link| link["address"].as_str().unwrap())
                    .collect();
                format!("{}: {}", object["reference"], addresses.join(" "))
            })
            .collect()
    };
    assert_eq!(links(&reread), links(&edited));
    let value = |editor: &Value| -> Value {
        editor["pages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|page| page["items"].as_array().unwrap().iter())
            .find(|item| item["reference"] == "P-1_R-1")
            .map(|item| item["value"].clone())
            .unwrap()
    };
    assert_eq!(value(&reread), "42");
    assert_eq!(value(&edited), "42");
    let (_, addresses) = call(&server, "GET", "/api/group-addresses", Value::Null).await;
    let name = |address: &str| -> (Value, Value) {
        let entry = addresses["addresses"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["address"] == address)
            .unwrap_or(&Value::Null);
        (entry["name"].clone(), entry["source"].clone())
    };
    assert_eq!(name("1/1/1"), (json!("Hall ceiling"), json!("project")));
    assert_eq!(name("1/1/9"), (json!("Hall night light"), json!("project")));
    assert_eq!(name("3/0/7"), (json!("Shed & <yard>"), json!("project")));

    // And written out once more with nothing changed, it is the same file again.
    let (_, twice) = request(
        &server,
        "GET",
        &format!("/api/projects/{again}/export"),
        b"",
    )
    .await;
    assert_eq!(twice, exported);
    let _ = std::fs::remove_dir_all(&directory);
}
