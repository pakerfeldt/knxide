//! Write the test kit's made-up project as a project archive:
//! `cargo run -p knxide-testkit --example demo_project -- demo.knxproj`.
//!
//! Everything in it is invented. It is what the planner's and the programmer's tests read, and
//! it lets the programmer's page be tried without a real project (with `--simulate`).

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: demo_project OUTPUT.knxproj");
        std::process::exit(2);
    };
    if let Err(error) = std::fs::write(&path, knxide_testkit::project::archive()) {
        eprintln!("{path}: {error}");
        std::process::exit(1);
    }
    println!("wrote {path}");
}
