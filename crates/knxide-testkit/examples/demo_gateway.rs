//! A fake gateway with live-looking bus traffic, for developing UIs without a real bus.
//!
//! ```sh
//! cargo run -p knxide-testkit --example demo_gateway -- [--listen 127.0.0.1:3671] [--project knx.xml]
//! ```
//!
//! With an ETS export, traffic uses its group addresses and DPTs: temperatures drift,
//! switches toggle, dimmers move, meters count up. Reads are answered with the last value.
//!
//! It also acts like actuators: a write to a command address ("Kök tak bryt", "… dim
//! absolut", "… bör") is reported back on its status address ("Kök tak status", "… status
//! värde", "… faktiskt bör"), found by name. Status addresses only change that way.

use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use knxide_core::dpt::{DptId, DptValue, ValueKind};
use knxide_core::{Apdu, GroupAddress, IndividualAddress, LData, ga};
use knxide_project::GroupAddressTable;
use knxide_testkit::FakeGateway;

/// xorshift64: good enough for fake traffic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn unit(&mut self) -> f32 {
        (self.next() % 10_000) as f32 / 10_000.0
    }
}

struct Point {
    ga: GroupAddress,
    dpt: DptId,
    source: IndividualAddress,
    value: f32,
}

fn main() {
    let mut listen: SocketAddr = "127.0.0.1:3671".parse().unwrap_or_else(|_| unreachable!());
    let mut project = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            ("--listen", Some(v)) => listen = v.parse().expect("--listen IP:PORT"),
            ("--project", Some(v)) => project = Some(v),
            _ => {
                eprintln!("usage: demo_gateway [--listen IP:PORT] [--project FILE]");
                std::process::exit(2);
            }
        }
    }

    let mut rng = Rng(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64)
        | 1);
    let mut points: Vec<Point> = match &project {
        Some(path) => {
            let table = GroupAddressTable::parse_bytes(&std::fs::read(path).expect("read project"))
                .expect("import project");
            table
                .iter()
                .filter_map(|e| Some((e.address, e.dpt?)))
                .filter(|(_, dpt)| dpt.is_supported())
                .map(|(ga, dpt)| Point {
                    ga,
                    dpt,
                    source: IndividualAddress::from_raw(
                        0x1100 + (u16::from(ga.main()) << 4) + u16::from(ga.middle()),
                    ),
                    value: 0.0,
                })
                .collect()
        }
        None => vec![
            Point {
                ga: ga!(1 / 1 / 10),
                dpt: DptId::new(1, 1),
                source: IndividualAddress::from_raw(0x1114),
                value: 0.0,
            },
            Point {
                ga: ga!(1 / 1 / 14),
                dpt: DptId::new(5, 1),
                source: IndividualAddress::from_raw(0x1114),
                value: 40.0,
            },
            Point {
                ga: ga!(4 / 1 / 30),
                dpt: DptId::new(9, 1),
                source: IndividualAddress::from_raw(0x1150),
                value: 4.0,
            },
            Point {
                ga: ga!(9 / 3 / 18),
                dpt: DptId::new(14, 56),
                source: IndividualAddress::from_raw(0x114B),
                value: 350.0,
            },
        ],
    };
    // Plausible starting values.
    for p in &mut points {
        p.value = match (p.dpt.main, p.dpt.sub) {
            (9, Some(1)) => 18.0 + rng.unit() * 6.0,
            (9, _) => rng.unit() * 100.0,
            (5, _) => (rng.unit() * 100.0).round(),
            (13 | 12, _) => 10_000.0 + rng.unit() * 1_000_000.0,
            (14, _) => 100.0 + rng.unit() * 2_000.0,
            _ => 0.0,
        };
    }

    // Command → status pairs. Those addresses change only when commanded.
    let table = match &project {
        Some(path) => GroupAddressTable::parse_bytes(&std::fs::read(path).expect("read project"))
            .expect("import"),
        None => GroupAddressTable::default(),
    };
    let links = status_links(&table);
    points.retain(|p| {
        !links
            .iter()
            .any(|(cmd, status)| *cmd == p.ga || *status == p.ga)
    });

    let gateway = FakeGateway::bind(listen).expect("bind");
    eprintln!(
        "Demo gateway on {} with {} changing addresses and {} simulated actuators. Ctrl-C to stop.",
        gateway.address(),
        points.len(),
        links.len()
    );
    // Actuators start off, at 0 %, or at 21 °C.
    for (command, status) in &links {
        let start = table.dpt(*command).and_then(|dpt| match dpt.value_kind()? {
            ValueKind::Bool { .. } => dpt.encode(&DptValue::Bool(false)).ok(),
            ValueKind::Float { .. } if dpt.main == 9 => dpt.encode(&DptValue::Float(21.0)).ok(),
            ValueKind::Float { .. } | ValueKind::Integer { .. } => {
                dpt.encode(&DptValue::Unsigned(0)).ok()
            }
            _ => None,
        });
        if let Some(payload) = start {
            gateway.set_value(*command, payload.clone());
            gateway.set_value(*status, payload);
        }
    }
    let mut last_command: Vec<Option<knxide_core::Payload>> = links
        .iter()
        .map(|(command, _)| gateway.value(*command))
        .collect();
    for p in &points {
        if let Some(payload) = value_for(p).and_then(|v| p.dpt.encode(&v).ok()) {
            gateway.set_value(p.ga, payload);
        }
    }

    let mut next_traffic = std::time::Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(50));
        // Actuators: report commands back on their status address.
        for (i, (command, status)) in links.iter().enumerate() {
            let now = gateway.value(*command);
            if now.is_some() && now != last_command[i] {
                last_command[i] = now.clone();
                let Some(payload) = now else { continue };
                gateway.set_value(*status, payload.clone());
                let mut ldata = LData::group(*status, Apdu::GroupValueWrite(payload));
                ldata.source = IndividualAddress::from_raw(0x1180);
                gateway.indicate(&ldata);
            }
        }
        if points.is_empty() || std::time::Instant::now() < next_traffic {
            continue;
        }
        next_traffic =
            std::time::Instant::now() + Duration::from_millis(150 + rng.below(1200) as u64);
        // A burst of 1–3 telegrams, like a device reporting several objects.
        for _ in 0..1 + rng.below(3) {
            let i = rng.below(points.len());
            let Some(p) = points.get_mut(i) else { continue };
            step(p, &mut rng);
            let Some(payload) = value_for(p).and_then(|v| p.dpt.encode(&v).ok()) else {
                continue;
            };
            gateway.set_value(p.ga, payload.clone());
            let mut ldata = LData::group(p.ga, Apdu::GroupValueWrite(payload));
            ldata.source = p.source;
            gateway.indicate(&ldata);
        }
    }
}

fn step(p: &mut Point, rng: &mut Rng) {
    p.value = match p.dpt.value_kind() {
        Some(ValueKind::Bool { .. }) => {
            if p.value > 0.5 {
                0.0
            } else {
                1.0
            }
        }
        Some(ValueKind::Float { min, max }) => {
            let span = if p.dpt.main == 9 {
                0.4
            } else {
                (max.min(1e6) - min.max(-1e6)) * 0.01
            };
            (p.value + (rng.unit() - 0.5) * span).clamp(min, max)
        }
        Some(ValueKind::Integer { min, max }) if p.dpt.main == 13 || p.dpt.main == 12 => {
            (p.value + rng.below(40) as f32).clamp(min as f32, max as f32)
        }
        Some(ValueKind::Integer { min, max }) => {
            (p.value + rng.below(11) as f32 - 5.0).clamp(min as f32, max.min(1_000_000) as f32)
        }
        _ => p.value,
    };
}

fn value_for(p: &Point) -> Option<DptValue> {
    Some(match p.dpt.value_kind()? {
        ValueKind::Bool { .. } => DptValue::Bool(p.value > 0.5),
        ValueKind::Float { .. } => DptValue::Float((p.value * 100.0).round() / 100.0),
        ValueKind::Integer { .. } => DptValue::Signed(p.value as i32),
        ValueKind::Step => DptValue::Step(knxide_core::dpt::Step {
            control: true,
            step_code: 1,
        }),
        ValueKind::TimeOfDay => {
            let secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
            DptValue::TimeOfDay(knxide_core::dpt::TimeOfDay {
                weekday: 0,
                hour: ((secs / 3600) % 24) as u8,
                minute: ((secs / 60) % 60) as u8,
                second: (secs % 60) as u8,
            })
        }
        ValueKind::Date => DptValue::Date(knxide_core::dpt::Date {
            year: 2026,
            month: 11,
            day: 1,
        }),
        ValueKind::Text { .. } => DptValue::String("knxide demo".into()),
        _ => return None,
    })
}

/// Pair command addresses with their status addresses by name, as an actuator would.
fn status_links(table: &GroupAddressTable) -> Vec<(GroupAddress, GroupAddress)> {
    const PAIRS: &[(&str, &str)] = &[
        (" bryt", " status"),
        ("", " status"),
        (" dim absolut", " status värde"),
        (" bör", " faktiskt bör"),
    ];
    let by_name: std::collections::HashMap<String, GroupAddress> = table
        .iter()
        .map(|e| (e.name.to_lowercase(), e.address))
        .collect();
    let mut links = Vec::new();
    for entry in table.iter() {
        let name = entry.name.to_lowercase();
        for (command_suffix, status_suffix) in PAIRS {
            let Some(base) = name.strip_suffix(command_suffix) else {
                continue;
            };
            if command_suffix.is_empty() && name.ends_with(" status") {
                continue;
            }
            if let Some(&status) = by_name.get(&format!("{base}{status_suffix}"))
                && status != entry.address
                && !links.iter().any(|(c, _)| *c == entry.address)
            {
                links.push((entry.address, status));
            }
        }
    }
    links
}
