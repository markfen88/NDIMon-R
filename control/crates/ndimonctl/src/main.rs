//! Operator diagnostics and control-plane status.
mod diag;
mod status;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn run() -> Result {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("--version") => println!("ndimonctl {}", env!("CARGO_PKG_VERSION")),
        Some("status") if args.len() == 1 => status::run()?,
        Some("diag") => diag::run(&args[1..])?,
        _ => return Err("usage: ndimonctl <status|diag collect --out DIR --secs N --peer IP --nic NIC [--proc UNIT]|diag analyze DIR [CHANNEL]>".into()),
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("ndimonctl: {error}");
        std::process::exit(1);
    }
}
