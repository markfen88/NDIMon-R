use crate::Result;
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const FIELDS: &[&str] = &[
    "ndir_video_cpu",
    "ndir_video_core",
    "ndir_recv_cpu",
    "ndir_audio_cpu",
    "app_cpu",
    "app_top_thread_cpu",
    "freq0",
    "freq4",
    "freq6",
    "temp_max_c",
    "netrx_softirq",
    "netrx_softirq_cpu4",
    "udp_in",
    "udp_inerr",
    "udp_rcvbuferr",
    "tcp_inseg",
    "tcp_retrans",
    "nic_rx_bytes",
    "nic_rx_packets",
    "nic_rx_errs",
    "nic_rx_drop",
    "psi_cpu_some_avg10",
    "psi_cpu_stall_us",
    "mem_avail_mb",
    "ndimon_pid",
];
const PACKET_FIELDS: &[&str] = &[
    "wall_ms",
    "peer_packets",
    "peer_bytes",
    "peer_max_gap_ms",
    "peer_udp",
    "peer_tcp",
];

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path.trim_start_matches('/'))).unwrap_or_default()
}
fn number(text: &str) -> u64 {
    text.trim().parse().unwrap_or(0)
}
fn numbers(text: &str) -> Vec<u64> {
    text.split_whitespace().map(number).collect()
}
fn cpu_times(text: &str) -> BTreeMap<usize, (u64, u64)> {
    text.lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once(' ')?;
            let cpu = name.strip_prefix("cpu")?.parse().ok()?;
            let v = numbers(rest);
            if v.len() < 5 {
                return None;
            }
            Some((cpu, (v.iter().take(8).sum(), v[3] + v[4])))
        })
        .collect()
}
fn snmp(text: &str, protocol: &str, keys: &[&str]) -> Vec<u64> {
    let lines: Vec<_> = text
        .lines()
        .filter(|s| s.starts_with(&format!("{protocol}:")))
        .collect();
    if lines.len() < 2 {
        return vec![0; keys.len()];
    }
    let header: Vec<_> = lines[0].split_whitespace().skip(1).collect();
    let values: Vec<_> = lines[1].split_whitespace().skip(1).map(number).collect();
    keys.iter()
        .map(|key| {
            header
                .iter()
                .position(|s| s == key)
                .and_then(|i| values.get(i))
                .copied()
                .unwrap_or(0)
        })
        .collect()
}
fn netdev(text: &str, nic: &str) -> Vec<u64> {
    text.lines()
        .find_map(|s| {
            let (name, rest) = s.trim().split_once(':')?;
            (name == nic).then(|| numbers(rest).into_iter().take(4).collect())
        })
        .filter(|v: &Vec<u64>| v.len() == 4)
        .unwrap_or_else(|| vec![0; 4])
}
fn softirq(text: &str) -> Vec<u64> {
    text.lines()
        .find_map(|s| s.trim().strip_prefix("NET_RX:").map(numbers))
        .unwrap_or_default()
}
fn psi(text: &str) -> (f64, u64) {
    let values: BTreeMap<_, _> = text
        .lines()
        .find(|s| s.starts_with("some "))
        .unwrap_or("")
        .split_whitespace()
        .filter_map(|s| s.split_once('='))
        .collect();
    (
        values
            .get("avg10")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0),
        values.get("total").map_or(0, |s| number(s)),
    )
}
#[derive(Debug, Clone)]
struct Thread {
    comm: String,
    ticks: u64,
    core: i32,
}
fn thread_stat(comm: String, text: &str) -> Option<Thread> {
    let (_, fields) = text.rsplit_once(") ")?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    Some(Thread {
        comm,
        ticks: number(fields.get(11)?) + number(fields.get(12)?),
        core: fields.get(36)?.parse().ok()?,
    })
}
fn threads(root: &Path, pid: &str) -> BTreeMap<String, Thread> {
    let mut out = BTreeMap::new();
    if let Ok(entries) = fs::read_dir(root.join(format!("proc/{pid}/task"))) {
        for entry in entries.flatten() {
            let comm = fs::read_to_string(entry.path().join("comm"))
                .unwrap_or_default()
                .trim()
                .to_owned();
            let stat = fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
            if let Some(row) = thread_stat(comm, &stat) {
                out.insert(entry.file_name().to_string_lossy().into_owned(), row);
            }
        }
    }
    out
}
#[derive(Debug)]
struct Sample {
    cpus: BTreeMap<usize, (u64, u64)>,
    irq: Vec<u64>,
    counters: Vec<u64>,
    psi: (f64, u64),
    threads: BTreeMap<String, Thread>,
    freq: [u64; 3],
    temp: f64,
    mem: u64,
}
impl Sample {
    fn read(root: &Path, nic: &str, pid: &str) -> Self {
        let sn = read(root, "proc/net/snmp");
        let mut counters = snmp(&sn, "Udp", &["InDatagrams", "InErrors", "RcvbufErrors"]);
        counters.extend(snmp(&sn, "Tcp", &["InSegs", "RetransSegs"]));
        counters.extend(netdev(&read(root, "proc/net/dev"), nic));
        let mut temp = 0;
        if let Ok(entries) = fs::read_dir(root.join("sys/class/thermal")) {
            for entry in entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("thermal_zone"))
            {
                temp = temp.max(number(
                    &fs::read_to_string(entry.path().join("temp")).unwrap_or_default(),
                ));
            }
        }
        Self {
            cpus: cpu_times(&read(root, "proc/stat")),
            irq: softirq(&read(root, "proc/softirqs")),
            counters,
            psi: psi(&read(root, "proc/pressure/cpu")),
            threads: threads(root, pid),
            freq: [0, 4, 6].map(|p| {
                number(&read(
                    root,
                    &format!("sys/devices/system/cpu/cpufreq/policy{p}/scaling_cur_freq"),
                )) / 1000
            }),
            temp: temp as f64 / 1000.0,
            mem: read(root, "proc/meminfo")
                .lines()
                .find_map(|s| {
                    s.strip_prefix("MemAvailable:")
                        .map(|s| number(s.split_whitespace().next().unwrap_or("0")) / 1024)
                })
                .unwrap_or(0),
        }
    }
    fn row(
        &self,
        previous: &Self,
        ncpu: usize,
        hz: f64,
        seconds: f64,
        wall: u128,
        pid: &str,
    ) -> Vec<String> {
        let mut row = vec![wall.to_string()];
        for i in 0..ncpu {
            let (total, idle) = self.cpus.get(&i).copied().unwrap_or_default();
            let (ptotal, pidle) = previous.cpus.get(&i).copied().unwrap_or_default();
            row.push(format!(
                "{:.1}",
                100.0
                    * (1.0
                        - (idle as f64 - pidle as f64) / (total as f64 - ptotal as f64).max(1.0))
            ));
        }
        let (mut video, mut recv, mut audio, mut app, mut top) = (0.0, 0.0, 0.0, 0.0, 0.0_f64);
        let mut core = -1;
        for (tid, t) in &self.threads {
            let ticks = previous.threads.get(tid).map_or(t.ticks, |t| t.ticks);
            let percent = (t.ticks as f64 - ticks as f64) / hz / seconds * 100.0;
            if t.comm.starts_with("ndir:video") {
                video += percent;
                core = t.core;
            } else if t.comm.starts_with("ndir:recv") {
                recv += percent;
            } else if t.comm.starts_with("ndir:audio") {
                audio += percent;
            } else if t.comm == "ndimon-r" {
                app += percent;
                top = top.max(percent);
            }
        }
        row.extend([
            format!("{video:.1}"),
            core.to_string(),
            format!("{recv:.1}"),
            format!("{audio:.1}"),
            format!("{app:.1}"),
            format!("{top:.1}"),
        ]);
        row.extend(self.freq.map(|f| f.to_string()));
        row.push(format!("{:.1}", self.temp));
        let irq: Vec<_> = self
            .irq
            .iter()
            .enumerate()
            .map(|(i, v)| {
                if previous.irq.is_empty() {
                    0
                } else {
                    *v as i128 - previous.irq.get(i).copied().unwrap_or(0) as i128
                }
            })
            .collect();
        row.extend([
            irq.iter().sum::<i128>().to_string(),
            irq.get(4).copied().unwrap_or(0).to_string(),
        ]);
        row.extend(
            self.counters
                .iter()
                .zip(&previous.counters)
                .map(|(a, b)| (*a as i128 - *b as i128).to_string()),
        );
        row.extend([
            self.psi.0.to_string(),
            (self.psi.1 as i128 - previous.psi.1 as i128).to_string(),
            self.mem.to_string(),
            pid.into(),
        ]);
        row
    }
}

#[derive(Debug, Default)]
struct Packets {
    packets: u64,
    bytes: u64,
    gap: f64,
    udp: u64,
    tcp: u64,
    last: Option<Instant>,
}
impl Packets {
    fn add(&mut self, bytes: &[u8], peer: [u8; 4], now: Instant) {
        if bytes.len() < 34 || bytes[26..30] != peer {
            return;
        }
        self.packets += 1;
        self.bytes += bytes.len() as u64;
        match bytes[23] {
            17 => self.udp += 1,
            6 => self.tcp += 1,
            _ => (),
        }
        if let Some(last) = self.last {
            self.gap = self
                .gap
                .max(now.duration_since(last).as_secs_f64() * 1000.0);
        }
        self.last = Some(now);
    }
    fn take(&mut self, wall: u128) -> Vec<String> {
        let row = vec![
            wall.to_string(),
            self.packets.to_string(),
            self.bytes.to_string(),
            format!("{:.2}", self.gap),
            self.udp.to_string(),
            self.tcp.to_string(),
        ];
        *self = Self {
            last: self.last,
            ..Self::default()
        };
        row
    }
}
#[derive(Debug)]
struct Capture {
    stop: Arc<AtomicBool>,
    packets: Arc<Mutex<Packets>>,
    thread: Option<JoinHandle<std::io::Result<()>>>,
}
impl Capture {
    fn start(nic: &str, peer: Ipv4Addr) -> Result<Self> {
        // Linux AF_PACKET; ETH_P_IP is passed in network byte order.
        let socket = Socket::new(
            Domain::from(17),
            Type::RAW,
            Some(Protocol::from(i32::from(0x0800_u16.to_be()))),
        )?;
        socket.bind_device(Some(nic.as_bytes()))?;
        socket.set_recv_buffer_size(8 << 20)?;
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        let stop = Arc::new(AtomicBool::new(false));
        let packets = Arc::new(Mutex::new(Packets::default()));
        let (done, shared) = (stop.clone(), packets.clone());
        let thread = thread::spawn(move || {
            let mut socket = &socket;
            let mut buffer = [0; 2048];
            while !done.load(Ordering::Relaxed) {
                match socket.read(&mut buffer) {
                    Ok(n) => shared.lock().expect("packet lock").add(
                        &buffer[..n],
                        peer.octets(),
                        Instant::now(),
                    ),
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock
                                | std::io::ErrorKind::TimedOut
                                | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        });
        Ok(Self {
            stop,
            packets,
            thread: Some(thread),
        })
    }
    fn finish(&mut self) -> Result {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().map_err(|_| "packet reader panicked")??;
        }
        Ok(())
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

#[derive(Debug)]
struct Options {
    out: PathBuf,
    seconds: u64,
    peer: Ipv4Addr,
    nic: String,
    process: String,
}
impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let (mut out, mut seconds, mut peer, mut nic) = (None, None, None, None);
        let mut process = "ndimon-r".to_owned();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let value = args.next().ok_or("collect option needs a value")?;
            match arg.as_str() {
                "--out" => out = Some(PathBuf::from(value)),
                "--secs" => seconds = Some(value.parse()?),
                "--peer" => peer = Some(value.parse()?),
                "--nic" => nic = Some(value.clone()),
                "--proc" => process = value.clone(),
                _ => return Err(format!("unknown collect option: {arg}").into()),
            }
        }
        let options = Self {
            out: out.ok_or("--out is required")?,
            seconds: seconds.ok_or("--secs is required")?,
            peer: peer.ok_or("--peer is required")?,
            nic: nic.ok_or("--nic is required")?,
            process,
        };
        if options.seconds == 0
            || options.nic.is_empty()
            || options.nic.len() > 15
            || options.nic.as_bytes().contains(&0)
            || options.process.starts_with('-')
        {
            return Err("invalid duration, interface or service".into());
        }
        Ok(options)
    }
}

pub fn run(args: &[String]) -> Result {
    let options = Options::parse(args)?;
    let hz = nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK)?
        .ok_or("clock tick rate unavailable")? as f64;
    let root = Path::new("/");
    let ncpu = cpu_times(&read(root, "proc/stat"))
        .keys()
        .max()
        .map_or(0, |i| i + 1);
    if ncpu == 0 {
        return Err("/proc/stat has no CPUs".into());
    }
    let mut capture = Capture::start(&options.nic, options.peer)?;
    fs::create_dir_all(&options.out)?;
    let mut sys = csv::Writer::from_path(options.out.join("sys.csv"))?;
    let mut pkt = csv::Writer::from_path(options.out.join("pkt.csv"))?;
    let mut header = vec!["wall_ms".into()];
    header.extend((0..ncpu).map(|i| format!("cpu{i}_busy")));
    header.extend(FIELDS.iter().map(|s| s.to_string()));
    sys.write_record(&header)?;
    pkt.write_record(PACKET_FIELDS)?;
    let mut previous = Sample::read(root, &options.nic, "0");
    let start = Instant::now();
    let mut previous_time = start;
    for second in 1..=options.seconds {
        thread::sleep(
            (start + Duration::from_secs(second)).saturating_duration_since(Instant::now()),
        );
        let now = Instant::now();
        let wall = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let output = Command::new("systemctl")
            .args(["show", "-p", "MainPID", "--value", &options.process])
            .output()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let pid = if text.trim().is_empty() {
            "0"
        } else {
            text.trim()
        };
        if !pid.bytes().all(|b| b.is_ascii_digit()) {
            return Err("systemctl returned an invalid PID".into());
        }
        let sample = Sample::read(root, &options.nic, pid);
        sys.write_record(sample.row(
            &previous,
            ncpu,
            hz,
            now.duration_since(previous_time).as_secs_f64(),
            wall,
            pid,
        ))?;
        pkt.write_record(capture.packets.lock().expect("packet lock").take(wall))?;
        sys.flush()?;
        pkt.flush()?;
        previous = sample;
        previous_time = now;
    }
    capture.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proc_parsers_preserve_kernel_field_indices_and_missing_defaults() {
        assert_eq!(
            cpu_times("cpu 1 2 3\ncpu4 1 2 3 4 5 6 7 8 99\n")[&4],
            (36, 9)
        );
        assert_eq!(
            snmp(
                "Udp: InErrors InDatagrams\nUdp: 7 9\n",
                "Udp",
                &["InDatagrams", "RcvbufErrors"]
            ),
            [9, 0]
        );
        assert_eq!(netdev("  en0: 100 2 3 4 5\n", "en0"), [100, 2, 3, 4]);
        assert_eq!(netdev("en0.1: 8 8 8 8", "en0"), [0; 4]);
        assert_eq!(softirq(" NET_RX: 1 2 3\n"), [1, 2, 3]);
        assert_eq!(psi("some avg10=1.25 avg60=0 total=123\n"), (1.25, 123));
        let mut fields = vec!["0"; 37];
        fields[11] = "10";
        fields[12] = "20";
        fields[36] = "6";
        let t = thread_stat(
            "ndir:video".into(),
            &format!("42 (a name) with parens) {}", fields.join(" ")),
        )
        .unwrap();
        assert_eq!((t.ticks, t.core), (30, 6));
        assert!(thread_stat("".into(), "truncated").is_none());
    }
    #[test]
    fn packet_filter_deltas_and_cross_second_gap_match_legacy() {
        let mut packets = Packets::default();
        let start = Instant::now();
        let mut frame = [0; 34];
        frame[26..30].copy_from_slice(&[192, 0, 2, 1]);
        frame[23] = 17;
        packets.add(&frame, [192, 0, 2, 2], start);
        assert_eq!(packets.packets, 0);
        packets.add(&frame, [192, 0, 2, 1], start);
        assert_eq!(packets.take(1), ["1", "1", "34", "0.00", "1", "0"]);
        frame[23] = 6;
        packets.add(&frame, [192, 0, 2, 1], start + Duration::from_millis(1200));
        assert_eq!(packets.take(2), ["2", "1", "34", "1200.00", "0", "1"]);
    }
    #[test]
    fn sample_row_has_legacy_columns_and_first_thread_delta_is_zero() {
        let temp = tempfile::tempdir().unwrap();
        let previous = Sample::read(temp.path(), "none", "0");
        let mut sample = Sample::read(temp.path(), "none", "0");
        sample.threads.insert(
            "1".into(),
            Thread {
                comm: "ndir:video".into(),
                ticks: 100,
                core: 4,
            },
        );
        let row = sample.row(&previous, 1, 100.0, 1.0, 123, "4");
        assert_eq!(row.len(), FIELDS.len() + 2);
        assert_eq!(&row[2..4], ["0.0", "4"]);
        sample.threads.get_mut("1").unwrap().ticks = 175;
        assert_eq!(sample.row(&sample, 1, 100.0, 1.0, 124, "4")[2], "0.0");
    }
}
