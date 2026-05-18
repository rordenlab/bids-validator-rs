use std::process::Command;

fn main() {
    let status = Command::new("python3")
        .arg("benches/validator_perf.py")
        .status()
        .expect("failed to run benches/validator_perf.py");
    std::process::exit(status.code().unwrap_or(1));
}
