//! dpkg executes these ELF hooks directly; no shell interpreter is involved.
use std::{env, path::Path};

fn main() {
    let args: Vec<String> = env::args().collect();
    let name = Path::new(&args[0]).file_name().unwrap().to_string_lossy();
    let result = if !nix::unistd::geteuid().is_root() {
        Err("package operations require root".into())
    } else if name == "ndimon-package" {
        match args.get(1).map(String::as_str) {
            Some("console-acquire") => ndimon_package::console(false),
            Some("console-release") => ndimon_package::console(true),
            _ => Err(
                "ndimon-package is invoked by dpkg; console-acquire|console-release only".into(),
            ),
        }
    } else {
        ndimon_package::hook(
            &env::var("DPKG_MAINTSCRIPT_NAME").unwrap_or_default(),
            &env::var("DPKG_MAINTSCRIPT_PACKAGE").unwrap_or_default(),
            args.get(1).map(String::as_str).unwrap_or_default(),
        )
    };
    if let Err(error) = result {
        eprintln!("ndimon-package: {error}");
        std::process::exit(1);
    }
}
