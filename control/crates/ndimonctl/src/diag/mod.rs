//! CSV diagnostics retain the collector's column names and analyzer's reports.
mod analyze;
mod collect;
use crate::Result;
use std::path::Path;

pub fn run(args: &[String]) -> Result {
    match args.first().map(String::as_str) {
        Some("collect") => collect::run(&args[1..]),
        Some("analyze") if (2..=3).contains(&args.len()) => {
            print!("{}", analyze::report(Path::new(&args[1]), args.get(2).map(String::as_str).unwrap_or("2"))?);
            Ok(())
        }
        _ => Err("usage: ndimonctl diag <collect --out DIR --secs N --peer IP --nic NIC [--proc UNIT]|analyze DIR [CHANNEL]>".into()),
    }
}
