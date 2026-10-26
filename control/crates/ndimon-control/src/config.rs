//! Front-door configuration. Socket activation takes precedence over `listen`.

use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: SocketAddr,
    pub public_dir: PathBuf,
    pub auth_file: PathBuf,
    pub decoder_socket: PathBuf,
    pub finder_socket: PathBuf,
    pub tessera_socket: PathBuf,
    pub cookie_secure: bool,
    pub state_dir: PathBuf,
    pub legacy_root: PathBuf,
    pub privd_socket: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
            public_dir: "/usr/share/ndimon/public".into(),
            auth_file: "/etc/ndimon-auth.json".into(),
            decoder_socket: "/tmp/ndi-decoder.sock".into(),
            finder_socket: "/run/ndimon/finder.sock".into(),
            tessera_socket: "/run/tessera/control.sock".into(),
            cookie_secure: false,
            state_dir: "/var/lib/ndimon/state".into(),
            legacy_root: "/".into(),
            privd_socket: ndimon_privd_proto::CONTROL_SOCKET.into(),
        }
    }
}

impl Config {
    pub fn from_args() -> Result<Self, String> {
        let mut cfg = Self::default();
        let mut args = std::env::args().skip(1);
        while let Some(key) = args.next() {
            if key == "--cookie-secure" {
                cfg.cookie_secure = true;
                continue;
            }
            if key == "--help" || key == "-h" {
                return Err(
                    "usage: ndimon-control [--listen IP:PORT] [--public-dir PATH] \
                    [--auth-file PATH] [--decoder-socket PATH] [--finder-socket PATH] \
                    [--tessera-socket PATH] [--state-dir PATH] [--legacy-root PATH] \
                    [--privd-socket PATH] [--cookie-secure]"
                        .into(),
                );
            }
            if !matches!(
                key.as_str(),
                "--listen"
                    | "--public-dir"
                    | "--auth-file"
                    | "--decoder-socket"
                    | "--finder-socket"
                    | "--tessera-socket"
                    | "--state-dir"
                    | "--legacy-root"
                    | "--privd-socket"
            ) {
                return Err(format!("unknown option: {key}"));
            }
            let value = args.next().ok_or_else(|| format!("{key} needs a value"))?;
            match key.as_str() {
                "--listen" => cfg.listen = value.parse().map_err(|_| "invalid --listen address")?,
                "--public-dir" => cfg.public_dir = value.into(),
                "--auth-file" => cfg.auth_file = value.into(),
                "--decoder-socket" => cfg.decoder_socket = value.into(),
                "--finder-socket" => cfg.finder_socket = value.into(),
                "--tessera-socket" => cfg.tessera_socket = value.into(),
                "--state-dir" => cfg.state_dir = value.into(),
                "--legacy-root" => cfg.legacy_root = value.into(),
                "--privd-socket" => cfg.privd_socket = value.into(),
                _ => unreachable!(),
            }
        }
        Ok(cfg)
    }
}
