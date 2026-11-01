//! Run the `knxide` binary against the fake gateway.

use std::io::{BufRead, BufReader};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use knxide_core::{Apdu, LData, Payload, ga};
use knxide_testkit::FakeGateway;

fn knxide(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_knxide"))
        .args(args)
        .env_remove("KNXIDE_GATEWAY")
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn write_then_read() {
    let gateway = FakeGateway::spawn().unwrap();
    let addr = gateway.address().to_string();

    let out = knxide(&[
        "write",
        "--gateway",
        &addr,
        "1/2/4",
        "21.5",
        "--dpt",
        "9.001",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        gateway.value(ga!(1 / 2 / 4)),
        Some(Payload::Bytes(vec![0x0C, 0x33]))
    );

    let out = knxide(&["read", "--gateway", &addr, "1/2/4", "--dpt", "9.001"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "21.5 °C  (0c 33)");
    assert!(
        stderr(&out).contains("Response from 1.1.10"),
        "{}",
        stderr(&out)
    );

    let out = knxide(&["write", "--gateway", &addr, "1/2/3", "on", "--dpt", "1.001"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(gateway.value(ga!(1 / 2 / 3)), Some(Payload::Small(1)));

    let out = knxide(&["write", "--gateway", &addr, "1/2/5", "0x0102"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        gateway.value(ga!(1 / 2 / 5)),
        Some(Payload::Bytes(vec![1, 2]))
    );
}

#[test]
fn write_over_tcp() {
    let gateway = FakeGateway::spawn_tcp().unwrap();
    let out = knxide(&[
        "write",
        "--tcp",
        "--gateway",
        &gateway.address().to_string(),
        "4/4/4",
        "1",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(gateway.value(ga!(4 / 4 / 4)), Some(Payload::Small(1)));
}

#[test]
fn read_without_response_fails() {
    let gateway = FakeGateway::spawn().unwrap();
    let out = knxide(&[
        "read",
        "--gateway",
        &gateway.address().to_string(),
        "7/7/7",
        "--timeout",
        "0.3",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("no response from 7/7/7"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn gateway_from_environment() {
    let gateway = FakeGateway::spawn().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_knxide"))
        .args(["write", "3/3/3", "1"])
        .env("KNXIDE_GATEWAY", gateway.address().to_string())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(gateway.value(ga!(3 / 3 / 3)), Some(Payload::Small(1)));
}

#[test]
fn discover_host() {
    let gateway = FakeGateway::spawn().unwrap();
    let out = knxide(&[
        "discover",
        "--host",
        &gateway.address().to_string(),
        "--timeout",
        "2",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("knxide fake gateway"), "{text}");
    assert!(text.contains("tunnelling v1"), "{text}");
}

#[test]
fn monitor_prints_frames() {
    let gateway = FakeGateway::spawn().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_knxide"))
        .args([
            "monitor",
            "--gateway",
            &gateway.address().to_string(),
            "--count",
            "2",
            "--dpt",
            "0/0/1=9.001",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Wait until it is connected.
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    assert!(line.starts_with("Connected as"), "{line}");

    let mut write = LData::group(
        ga!(0 / 0 / 1),
        Apdu::GroupValueWrite(Payload::Bytes(vec![0x0C, 0x1A])),
    );
    write.source = "1.1.5".parse().unwrap();
    gateway.indicate(&write);
    let mut read = LData::group(ga!(0 / 0 / 2), Apdu::GroupValueRead);
    read.source = "1.1.6".parse().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    gateway.indicate(&read);

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(
        lines[0].ends_with("1.1.5      -> 0/0/1      write 21 °C  (0c 1a)"),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].ends_with("1.1.6      -> 0/0/2      read"),
        "{}",
        lines[1]
    );
}

#[test]
fn project_file_supplies_types_and_names() {
    let gateway = FakeGateway::spawn().unwrap();
    let addr = gateway.address().to_string();
    let dir = std::env::temp_dir().join(format!("knxide-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let project = dir.join("ga.csv");
    std::fs::write(
        &project,
        "\"Group name\";\"Address\";\"DatapointType\"\n\"Living temperature\";\"1/2/4\";\"DPST-9-1\"\n",
    )
    .unwrap();
    let project = project.to_str().unwrap();

    let out = knxide(&[
        "write",
        "--gateway",
        &addr,
        "--project",
        project,
        "1/2/4",
        "22.5",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = knxide(&["read", "--gateway", &addr, "--project", project, "1/2/4"]);
    assert_eq!(stdout(&out).trim(), "22.5 °C  (0c 65)");

    let mut child = Command::new(env!("CARGO_BIN_EXE_knxide"))
        .args([
            "monitor",
            "--gateway",
            &addr,
            "--project",
            project,
            "--count",
            "1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stderr.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let mut write = LData::group(
        ga!(1 / 2 / 4),
        Apdu::GroupValueWrite(Payload::Bytes(vec![0x0C, 0x1A])),
    );
    write.source = "1.1.5".parse().unwrap();
    gateway.indicate(&write);
    let text = stdout(&child.wait_with_output().unwrap());
    assert!(
        text.trim_end()
            .ends_with("1/2/4      Living temperature  write 21 °C  (0c 1a)"),
        "{text}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn restart() {
    let gateway = FakeGateway::spawn().unwrap();
    let addr = gateway.address().to_string();
    let out = knxide(&["restart", "--gateway", &addr, "1.1.10", "--yes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(gateway.restarts(), ["1.1.10".parse().unwrap()]);

    let out = knxide(&["restart", "--gateway", &addr, "1.1.99", "--yes"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("no device answers at 1.1.99"),
        "{}",
        stderr(&out)
    );

    // No terminal and no --yes: refuse.
    let out = knxide(&["restart", "--gateway", &addr, "1.1.10"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(gateway.restarts().len(), 1);
}

#[test]
fn usage_errors() {
    for args in [
        &["bogus"][..],
        &["write", "--gateway", "127.0.0.1", "1/2/3"],
        &["write", "--gateway", "127.0.0.1", "99/0/0", "1"],
        &[
            "write",
            "--gateway",
            "127.0.0.1",
            "1/2/3",
            "hot",
            "--dpt",
            "9.001",
        ],
        &["read", "--gateway", "127.0.0.1", "1/2/3", "--dpt", "19.001"],
        &["read", "--frobnicate", "1", "1/2/3"],
        &["restart", "--gateway", "127.0.0.1", "1/2/3", "--yes"],
    ] {
        let out = knxide(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
    }
    let out = knxide(&["help"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("USAGE"));
}
