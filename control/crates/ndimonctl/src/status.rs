use crate::Result;
use std::{fs, process::Command};

pub fn run() -> Result {
    println!("=== NDIMon-R Services ===");
    let system = Command::new("systemctl")
        .args(["is-enabled", "ndimon-r"])
        .output()?
        .status
        .success();
    let mut command = Command::new("systemctl");
    if !system {
        command.arg("--user");
    }
    let output = command
        .args([
            "status",
            "ndimon-r",
            "ndimon-finder",
            "ndimon-control",
            "ndimon-privd",
            "--no-pager",
            "-l",
        ])
        .output()?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    println!("\n=== Health (loopback, no auth) ===");
    match Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--max-time",
            "3",
            "http://127.0.0.1/api/health",
        ])
        .output()
    {
        Ok(output) if output.status.success() => print_json(&output.stdout),
        _ => println!("(API not reachable on port 80)"),
    }
    println!("\n=== Sources ===");
    match fs::read("/var/lib/ndimon/ndimon-sources.json") {
        Ok(bytes) => print_json(&bytes),
        Err(_) => println!("(no /var/lib/ndimon/ndimon-sources.json)"),
    }
    Ok(())
}

fn print_json(bytes: &[u8]) {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(value) => println!(
            "{}",
            serde_json::to_string_pretty(&value).expect("JSON value")
        ),
        Err(_) => println!("{}", String::from_utf8_lossy(bytes)),
    }
}
