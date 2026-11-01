//! List the group addresses in an ETS export: `cargo run -p knxide-project --example list -- FILE`.

use std::collections::BTreeMap;

use knxide_project::GroupAddressTable;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: list FILE");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("cannot read {path}: {e}");
        std::process::exit(1);
    });
    let table = GroupAddressTable::parse_bytes(&bytes).unwrap_or_else(|e| {
        eprintln!("cannot import {path}: {e}");
        std::process::exit(1);
    });
    let mut by_dpt: BTreeMap<String, (usize, bool)> = BTreeMap::new();
    for entry in table.iter() {
        let dpt = entry.dpt.map_or_else(|| "-".to_string(), |d| d.to_string());
        let supported = entry.dpt.is_some_and(|d| d.is_supported());
        println!(
            "{:<10} {:<8} {}  [{}]",
            entry.address,
            dpt,
            entry.name,
            entry.path.join(" / ")
        );
        let slot = by_dpt.entry(dpt).or_insert((0, supported));
        slot.0 += 1;
    }
    eprintln!("\n{} group addresses", table.len());
    for (dpt, (count, supported)) in by_dpt {
        eprintln!(
            "  {dpt:<8} {count:>4}{}",
            if supported { "" } else { "  (not supported)" }
        );
    }
}
