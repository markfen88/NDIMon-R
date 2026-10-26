use crate::Result;
use std::{collections::BTreeMap, fmt::Write, path::Path};

#[derive(Debug)]
struct Table {
    columns: BTreeMap<String, usize>,
    rows: Vec<csv::StringRecord>,
}
impl Table {
    fn read(path: &Path) -> Result<Self> {
        let mut reader = csv::Reader::from_path(path)?;
        let columns = reader
            .headers()?
            .iter()
            .enumerate()
            .map(|(i, key)| (key.into(), i))
            .collect();
        let rows = reader
            .records()
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows.is_empty() {
            return Err(format!("{} contains no samples", path.display()).into());
        }
        Ok(Self { columns, rows })
    }
    fn text(&self, row: usize, key: &str) -> Result<&str> {
        self.columns
            .get(key)
            .and_then(|i| self.rows[row].get(*i))
            .ok_or_else(|| format!("missing CSV column {key}").into())
    }
    fn value(&self, row: usize, key: &str) -> Result<f64> {
        let value: f64 = self.text(row, key)?.parse()?;
        if !value.is_finite() {
            return Err(format!("non-finite CSV value in {key}").into());
        }
        Ok(value)
    }
    fn values(&self, key: &str) -> Result<Vec<f64>> {
        (0..self.rows.len()).map(|i| self.value(i, key)).collect()
    }
}
fn percentile(values: &[f64], p: f64) -> Result<f64> {
    if values.is_empty() {
        return Err(
            "diagnostics require steady samples after five seconds and at least one quiet sample"
                .into(),
        );
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Ok(sorted[((sorted.len() as f64 * p) as usize).min(sorted.len() - 1)])
}
fn median(values: &[f64]) -> Result<f64> {
    let high = percentile(values, 0.5)?;
    if values.len().is_multiple_of(2) {
        Ok((high + percentile(values, 0.5 - 0.5 / values.len() as f64)?) / 2.0)
    } else {
        Ok(high)
    }
}
fn max(values: &[f64]) -> f64 {
    values.iter().copied().reduce(f64::max).unwrap_or(0.0)
}

pub fn report(root: &Path, channel: &str) -> Result<String> {
    if channel.is_empty() || !channel.bytes().all(|b| b.is_ascii_digit()) {
        return Err("channel must be a number".into());
    }
    let t = Table::read(&root.join(format!("tele/t-ch{channel}.csv")))?;
    let s = Table::read(&root.join("sys.csv"))?;
    let p = Table::read(&root.join("pkt.csv"))?;
    let t0 = t.value(0, "wall_ms")?;
    let mut previous = t.value(0, "sdk_dropped")?;
    let mut rows = Vec::new();
    for i in 0..t.rows.len() {
        let dropped = t.value(i, "sdk_dropped")?;
        rows.push((
            (t.value(i, "wall_ms")? - t0) / 1000.0,
            dropped - previous,
            t.value(i, "ts_gap_max_ms")?,
            t.value(i, "age_avg_ms")?,
            t.value(i, "age_max_ms")?,
        ));
        previous = dropped;
    }
    let steady: Vec<_> = rows.iter().filter(|r| r.0 > 5.0).collect();
    let mut out = String::new();
    let last = t.rows.len() - 1;
    writeln!(
        out,
        "received {}  dropped {}  drop seconds (after 5 s): {}",
        t.text(last, "sdk_total")?,
        t.text(last, "sdk_dropped")?,
        steady.iter().filter(|r| r.1 > 0.0).count()
    )?;
    let mut bursts: Vec<(f64, f64, f64)> = Vec::new();
    let mut last_drop = -99.0;
    for r in &steady {
        if r.1 > 0.0 {
            if r.0 - last_drop > 5.0 {
                bursts.push((r.0, r.0, 0.0));
            }
            let burst = bursts.last_mut().expect("drop starts burst");
            burst.1 = r.0;
            burst.2 += r.1;
            last_drop = r.0;
        }
    }
    let bursts: Vec<_> = bursts
        .iter()
        .map(|b| {
            format!(
                "({:.0}, {:.0}, {:.0})",
                b.0.round_ties_even(),
                b.1.round_ties_even(),
                b.2.round_ties_even()
            )
        })
        .collect();
    writeln!(
        out,
        "bursts (start s, end s, drops): [{}]",
        bursts.join(", ")
    )?;
    let big: Vec<_> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.2 > 150.0 && r.0 > 5.0)
        .collect();
    let adjacent = big
        .iter()
        .filter(|(i, _)| {
            rows[i.saturating_sub(1)..(i + 2).min(rows.len())]
                .iter()
                .any(|r| r.1 > 0.0)
        })
        .count();
    writeln!(
        out,
        "sender-timestamp gaps >150 ms: {}; with drops adjacent: {adjacent}; largest {:.0} ms",
        big.len(),
        big.iter().map(|(_, r)| r.2).reduce(f64::max).unwrap_or(0.0)
    )?;
    let ages: Vec<_> = steady.iter().map(|r| r.3).collect();
    let base = median(&ages)?;
    let quiet: Vec<_> = steady
        .iter()
        .filter(|r| r.1 == 0.0)
        .map(|r| r.3 - base)
        .collect();
    writeln!(
        out,
        "frame age (local wall minus sender timestamp; offset {base:.0} ms is clock skew): variation p50 {:+.1} p95 {:+.1} p99 {:+.1} max {:+.1} ms",
        percentile(&quiet, 0.5)?,
        percentile(&quiet, 0.95)?,
        percentile(&quiet, 0.99)?,
        max(&quiet)
    )?;
    writeln!(
        out,
        "seconds with age_max >120 ms over the median: {}",
        steady.iter().filter(|r| r.4 - base > 120.0).count()
    )?;
    let gaps = p.values("peer_max_gap_ms")?;
    writeln!(
        out,
        "packets from peer: max gap per second p50 {:.1} p99 {:.1} max {:.1} ms",
        percentile(&gaps, 0.5)?,
        percentile(&gaps, 0.99)?,
        max(&gaps)
    )?;
    let mut sums = Vec::new();
    for key in [
        "udp_inerr",
        "udp_rcvbuferr",
        "nic_rx_errs",
        "nic_rx_drop",
        "tcp_retrans",
    ] {
        sums.push(s.values(key)?.iter().sum::<f64>());
    }
    writeln!(
        out,
        "UDP in-errors {:.0} rcvbuf errors {:.0} NIC rx errs {:.0} NIC rx drop {:.0} TCP retrans {:.0}",
        sums[0], sums[1], sums[2], sums[3], sums[4]
    )?;
    for key in ["ndir_video_cpu", "app_cpu", "temp_max_c"] {
        let v = s.values(key)?;
        writeln!(
            out,
            "{key}: p50 {:.1} p99 {:.1} max {:.1}",
            percentile(&v, 0.5)?,
            percentile(&v, 0.99)?,
            max(&v)
        )?;
    }
    let queue_median = median(&t.values("queue_drops")?)?;
    let queue = if t.rows.len().is_multiple_of(2) {
        format!("{queue_median:.1}")
    } else {
        format!("{queue_median:.0}")
    };
    writeln!(
        out,
        "render max ms p99 {:.1}; flip gap max ms p99 {:.1}; display-queue drops/s median {queue}",
        percentile(&t.values("render_max_ms")?, 0.99)?,
        percentile(&t.values("flip_gap_max_ms")?, 0.99)?
    )?;
    let mut cores: Vec<(i32, usize)> = Vec::new();
    let (mut busy, mut slow) = (0, 0);
    for i in 0..s.rows.len() {
        let core = s.value(i, "ndir_video_core")? as i32;
        if let Some(row) = cores.iter_mut().find(|row| row.0 == core) {
            row.1 += 1;
        } else {
            cores.push((core, 1));
        }
        if s.value(i, "ndir_video_cpu")? > 60.0 {
            busy += 1;
            let key = match core {
                4 | 5 => "freq4",
                6 | 7 => "freq6",
                _ => "freq0",
            };
            if s.value(i, key)? < 1200.0 {
                slow += 1;
            }
        }
    }
    cores.sort_by_key(|a| std::cmp::Reverse(a.1));
    let cores: Vec<_> = cores
        .iter()
        .take(4)
        .map(|(core, n)| format!("({core}, {n})"))
        .collect();
    writeln!(out, "SDK video thread cores: [{}]", cores.join(", "))?;
    writeln!(
        out,
        "seconds the SDK video thread was >60% busy while its own cluster sampled below 1200 MHz: {slow} of {busy}"
    )?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn legacy_report_fixture_is_byte_identical() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/diag");
        assert_eq!(
            report(&root, "2").unwrap(),
            fs::read_to_string(root.join("expected.txt")).unwrap()
        );
    }
    #[test]
    fn malformed_empty_and_unsafe_channel_are_errors() {
        let root = tempfile::tempdir().unwrap();
        assert!(report(root.path(), "../../etc/passwd").is_err());
        fs::create_dir(root.path().join("tele")).unwrap();
        fs::write(root.path().join("tele/t-ch2.csv"), "wall_ms\n").unwrap();
        assert!(report(root.path(), "2").is_err());
        assert!(percentile(&[], 0.99).is_err());
        assert_eq!(median(&[1.0, 2.0, 4.0, 8.0]).unwrap(), 3.0);
    }
}
