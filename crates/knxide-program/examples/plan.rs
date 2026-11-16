//! Print what a download would send, without a bus:
//! `cargo run -p knxide-program --example plan -- APP.xml DEVICE.xml ADDRESSES [full|partial]`.
//!
//! `APP.xml` is an application program file, `DEVICE.xml` holds the device's `DeviceInstance`
//! element (a whole project file works too: the first device is taken), `ADDRESSES` is either a
//! project file with `GroupAddress` elements or a list of `GA-<n> <address>` lines.
//!
//! The plan is made against a modelled device that holds the application and has empty memory,
//! so it shows the shape of a download, not what a particular device would answer.

use knxide_program::{
    ApplicationProgram, DeviceData, Download, DownloadKind, GroupAddresses, PlanOptions,
    SimulatedDevice,
};

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [app, device, addresses, rest @ ..] = arguments.as_slice() else {
        eprintln!("usage: plan APP.xml DEVICE.xml ADDRESSES [full|partial]");
        std::process::exit(2);
    };
    let read = |path: &String| {
        std::fs::read_to_string(path).unwrap_or_else(|error| fail(&format!("{path}: {error}")))
    };
    let program = ApplicationProgram::parse(&read(app)).unwrap_or_else(|e| fail(&e.to_string()));
    let device = DeviceData::parse(&read(device)).unwrap_or_else(|e| fail(&e.to_string()));
    let addresses = read(addresses);
    let addresses = if addresses.trim_start().starts_with('<') {
        GroupAddresses::parse_project(&addresses)
    } else {
        GroupAddresses::parse_list(&addresses)
    }
    .unwrap_or_else(|e| fail(&e.to_string()));
    let download = match rest.first().map(String::as_str) {
        Some("full") => Download::build(&program, &device, &addresses, DownloadKind::FULL),
        Some("partial") => Download::build(&program, &device, &addresses, DownloadKind::PARTIAL),
        Some(other) => fail(&format!("unknown kind {other:?}")),
        None => Download::prepare(&program, &device, &addresses),
    }
    .unwrap_or_else(|e| fail(&e.to_string()));
    println!(
        "# mask {:04X}, {} download{}",
        download.profile.mask_version,
        if download.kind.full {
            "full"
        } else {
            "partial"
        },
        if download.fallback.is_some() {
            " (full if the device does not hold the application)"
        } else {
            ""
        }
    );
    println!("# memory to send");
    for run in download.image.runs(0, 0x1_0000) {
        let hex: String = run.data.iter().map(|byte| format!("{byte:02X}")).collect();
        println!("{:04X} {hex}", run.address);
    }
    println!("# requests");
    let mut model = SimulatedDevice::holding(&download);
    let (requests, failure) = match download.plan(&mut model, &PlanOptions::default()) {
        Ok(plan) => (plan.requests, None),
        Err(failure) => (failure.requests, Some(failure.error)),
    };
    for request in &requests {
        println!("{request}");
    }
    if let Some(error) = failure {
        fail(&format!("planning stopped: {error}"));
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}
