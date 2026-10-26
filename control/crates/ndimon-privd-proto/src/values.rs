use serde::{Deserialize, Serialize};
use std::{fmt, net::Ipv4Addr};

macro_rules! string_value {
    ($name:ident, $check:expr, $reason:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl TryFrom<String> for $name {
            type Error = String;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                if ($check)(&value) {
                    Ok(Self(value))
                } else {
                    Err($reason.into())
                }
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}
fn label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.as_bytes()[s.len() - 1].is_ascii_alphanumeric()
}
fn plugin(s: &str) -> bool {
    (2..=32).contains(&s.len())
        && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
string_value!(
    Hostname,
    label,
    "hostname must be one DNS label, 1..63 bytes"
);
string_value!(
    NtpServer,
    |s: &str| {
        if s.split('.').count() == 4 && s.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            return s.parse::<Ipv4Addr>().is_ok();
        }
        s.len() <= 253 && s.split('.').all(label)
    },
    "NTP server must be IPv4 or a DNS hostname"
);
string_value!(
    PluginName,
    plugin,
    "plugin name must match [a-z][a-z0-9-]{1,31}"
);
string_value!(
    PluginCommandName,
    plugin,
    "plugin command must match [a-z][a-z0-9-]{1,31}"
);
string_value!(
    NetworkId,
    |s: &str| !s.is_empty()
        && s.len() <= 32
        && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
    "invalid network id"
);
string_value!(
    InterfaceName,
    |s: &str| !s.is_empty()
        && s.len() <= 15
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
    "invalid physical interface"
);
string_value!(
    FlowId,
    |s: &str| !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
    "invalid flow id"
);
string_value!(
    NetworkToken,
    |s: &str| s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
    "token must be 64 lowercase hex characters"
);
string_value!(
    DebianVersion,
    |s: &str| !s.is_empty()
        && s.len() <= 128
        && s.as_bytes()[0].is_ascii_digit()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+:~-".contains(&b)),
    "invalid Debian version"
);
// Debian policy: [epoch:]upstream[-revision]. Keep versions out of argv/path
// syntax; colons are permitted in upstream only when an epoch is present.
fn release_version(s: &str) -> bool {
    if s.is_empty() || s.len() > 128 || !s.is_ascii() {
        return false;
    }
    let (upstream, epoch) = match s.split_once(':') {
        Some((epoch, rest)) => {
            if epoch.is_empty()
                || !epoch.bytes().all(|b| b.is_ascii_digit())
                || epoch.parse::<u32>().is_err()
            {
                return false;
            }
            (rest, true)
        }
        None => (s, false),
    };
    let upstream = if let Some((upstream, revision)) = upstream.rsplit_once('-') {
        if revision.is_empty()
            || !revision
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+.~".contains(&b))
        {
            return false;
        }
        upstream
    } else {
        upstream
    };
    !upstream.is_empty()
        && upstream.as_bytes()[0].is_ascii_digit()
        && upstream
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+~-".contains(&b) || epoch && b == b':')
}
string_value!(
    ReleaseVersion,
    release_version,
    "invalid Debian release version"
);
// Identifiers refer to files below fixed roots. They are never paths.
string_value!(
    ArchiveId,
    |s: &str| !s.is_empty()
        && s.len() <= 200
        && s.ends_with(".tar.gz")
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && !s.contains("..")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
    "invalid archive id"
);
string_value!(
    ImportId,
    |s: &str| !s.is_empty()
        && s.len() <= 100
        && !s.contains("..")
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
    "invalid import id"
);
string_value!(
    PluginJob,
    |s: &str| {
        let Some(tail) = s
            .strip_prefix("ndimon-plugin-")
            .and_then(|s| s.strip_suffix(".service"))
        else {
            return false;
        };
        let Some((name, number)) = tail.rsplit_once('-') else {
            return false;
        };
        plugin(name)
            && !number.is_empty()
            && number.len() <= 20
            && number.bytes().all(|b| b.is_ascii_digit())
    },
    "invalid plugin job unit"
);
string_value!(
    PluginUnit,
    |s: &str| s.len() <= 200
        && s.ends_with(".service")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_@.-".contains(&b)),
    "invalid plugin unit"
);
string_value!(
    WallTime,
    |s: &str| s.len() == 5
        && s.as_bytes()[2] == b':'
        && s.bytes()
            .enumerate()
            .all(|(i, b)| i == 2 || b.is_ascii_digit())
        && s[..2].parse::<u8>().is_ok_and(|v| v < 24)
        && s[3..].parse::<u8>().is_ok_and(|v| v < 60),
    "time must be HH:MM"
);

macro_rules! bounded {
    ($name:ident, $lo:literal, $hi:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "u16", into = "u16")]
        pub struct $name(u16);
        impl TryFrom<u16> for $name {
            type Error = String;
            fn try_from(v: u16) -> Result<Self, String> {
                if ($lo..=$hi).contains(&v) {
                    Ok(Self(v))
                } else {
                    Err(format!("{} must be {}..{}", stringify!($name), $lo, $hi))
                }
            }
        }
        impl From<$name> for u16 {
            fn from(v: $name) -> Self {
                v.0
            }
        }
        impl $name {
            pub fn get(self) -> u16 {
                self.0
            }
        }
    };
}
bounded!(VlanId, 1, 4094);
bounded!(UdpPort, 1024, 65535);
bounded!(ChannelCount, 1, 16);
bounded!(FirstChannel, 1, 65535);
bounded!(FramesPerPacket, 1, 1024);
