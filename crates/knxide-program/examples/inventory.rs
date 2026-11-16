//! List the devices of a project and say which of them could be downloaded:
//! `cargo run --release -p knxide-program --example inventory -- PROJECT.knxproj`.
//!
//! Nothing is sent anywhere; the project is only read. A password-protected project is refused.

use knxide_program::{Project, ProjectArchive};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: inventory PROJECT.knxproj");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&path).unwrap_or_else(|error| fail(&format!("{path}: {error}")));
    let archive = ProjectArchive::open(&bytes).unwrap_or_else(|error| fail(&error.to_string()));
    let project = Project::load(&archive).unwrap_or_else(|error| fail(&error.to_string()));

    println!(
        "{} ({}): {} devices, {} group addresses",
        project.name,
        project.id,
        project.devices.len(),
        project.group_addresses.len()
    );
    let mut supported = 0;
    for device in &project.devices {
        let address = device
            .address
            .map_or_else(|| "-".to_string(), |address| address.to_string());
        let order = device
            .product
            .as_ref()
            .map_or("-", |product| product.order_number.as_str());
        let (mask, application) = match &device.application {
            Some(program) => (
                format!("{:04X}", program.mask_version),
                format!("{} ({})", program.name, program.id),
            ),
            None => ("-".into(), "-".into()),
        };
        println!("{address:<9} {order:<16} {mask:<5} {application}");
        if device.is_supported() {
            supported += 1;
            if let (Some(download), Some(plan)) = (&device.download, &device.preview) {
                println!(
                    "          supported: {} download, {} requests against a modelled device",
                    if download.kind.full {
                        "full"
                    } else {
                        "partial (full if the device does not hold the application)"
                    },
                    plan.requests.len()
                );
            }
        }
        for obstacle in &device.obstacles {
            println!("          not supported: {obstacle}");
        }
    }
    println!("{supported} of {} devices supported", project.devices.len());
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}
