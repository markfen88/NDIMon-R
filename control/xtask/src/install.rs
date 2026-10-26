//! On-target dependency provisioning and installation of the exact CI packages.
//! Vendor SDK installers are read as archives; no downloaded shell is executed.
use crate::{Result, checked, package};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Cursor,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::Command,
};

const SDK_URL: &str = "https://downloads.ndi.tv/SDK/NDI_SDK_Linux/Install_NDI_SDK_v6_Linux.tar.gz";
const BUILD_DEPS: &[&str] = &[
    "build-essential",
    "cmake",
    "pkg-config",
    "git",
    "curl",
    "ca-certificates",
    "gnupg",
    "dpkg-dev",
    "libdrm-dev",
    "libasound2-dev",
    "libavahi-client-dev",
    "libavahi-common-dev",
    "nlohmann-json3-dev",
    "libsystemd-dev",
    "libavcodec-dev",
    "libavformat-dev",
    "libswscale-dev",
    "libavutil-dev",
    "libswresample-dev",
    "iputils-arping",
    "nftables",
    "libcap2-bin",
];
/// Nothing needs these: `libz-sys` compiles its bundled zlib when the headers are
/// absent. A mirror that lags a security update makes `zlib1g-dev` uninstallable
/// (`Depends: zlib1g (= …) but … is to be installed`, HW-1 H9) and apt then fails
/// the whole transaction, so they are installed on their own, best effort.
const OPTIONAL_BUILD_DEPS: &[&str] = &["zlib1g-dev"];
/// Ubuntu's `plucky` carries FFmpeg 7, whose `libavcodec.so.61` the NDI SDK's own
/// HX decoder loads; `noble` ships FFmpeg 6.1 and nothing else of plucky is wanted.
/// The source exists only while those two runtime libraries are installed, and
/// pinned below every other source so that no package noble provides — above all
/// the `-dev` packages the build compiles against — is replaced.
const FFMPEG_SOURCE: &str = "/etc/apt/sources.list.d/ndimon-ffmpeg-temporary.list";
const FFMPEG_PIN: &str = "/etc/apt/preferences.d/ndimon-ffmpeg-temporary";
const FFMPEG_PIN_TEXT: &str = "Package: *\nPin: release n=plucky\nPin-Priority: 100\n";
const FFMPEG_DEV: &[&str] = &[
    "libavcodec-dev",
    "libavformat-dev",
    "libswscale-dev",
    "libavutil-dev",
    "libswresample-dev",
];
pub fn apt(root: &Path, args: &[&str]) -> Result {
    let mut argv = vec![
        "flock",
        "/run/lock/ci-apt",
        "apt-get",
        "-o",
        "DPkg::Lock::Timeout=600",
    ];
    argv.extend(args);
    checked(root, "sudo", &argv)
}
fn sudo(root: &Path, program: &str, args: &[&str]) -> Result {
    let mut argv = vec![program];
    argv.extend(args);
    checked(root, "sudo", &argv)
}
fn text(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program).args(args).output()?;
    if !output.status.success() {
        return Err(format!("{program} failed").into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
pub(crate) fn download(url: &str, dest: &Path) -> Result {
    checked(
        Path::new("/"),
        "curl",
        &[
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "-fL",
            "--retry",
            "2",
            "--connect-timeout",
            "15",
            "--max-time",
            "600",
            "-o",
            dest.to_str().ok_or("download path")?,
            url,
        ],
    )
}
fn files(root: &Path, out: &mut Vec<PathBuf>) -> Result {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            files(&entry.path(), out)?;
        } else if entry.file_type()?.is_file() {
            out.push(entry.path());
        }
    }
    Ok(())
}
fn library_matches(arch: &str, path: &Path) -> bool {
    let value = path.to_string_lossy();
    match arch {
        "amd64" => value.contains("x86_64-linux-gnu"),
        "arm64" => value.contains("aarch64") || value.contains("arm64"),
        "armhf" => value.contains("arm-") && !value.contains("aarch64"),
        _ => false,
    }
}
fn archive_offset(data: &[u8]) -> Result<usize> {
    let marker = b"__NDI_ARCHIVE_BEGIN__\n";
    let offset = data
        .windows(marker.len())
        .position(|w| w == marker)
        .ok_or("NDI archive marker missing")?;
    if offset != 0 && data[offset - 1] != b'\n' {
        return Err("invalid SDK archive marker".into());
    }
    Ok(offset + marker.len())
}
pub fn sdk(prefix: &Path, arch: &str) -> Result {
    let temporary = tempfile::tempdir()?;
    let archive = temporary.path().join("ndi.tar.gz");
    download(SDK_URL, &archive)?;
    let bytes = fs::read(&archive)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    if let Ok(pin) = std::env::var("NDI_SDK_SHA256") {
        if pin.len() != 64 || !pin.eq_ignore_ascii_case(&hash) {
            return Err("NDI SDK checksum mismatch".into());
        }
    }
    let outer = temporary.path().join("outer");
    fs::create_dir(&outer)?;
    tar::Archive::new(GzDecoder::new(Cursor::new(bytes))).unpack(&outer)?;
    let mut entries = Vec::new();
    files(&outer, &mut entries)?;
    let installer = entries
        .iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.starts_with("Install_NDI_SDK") && s.ends_with(".sh"))
        })
        .ok_or("NDI SDK installer missing")?;
    let bytes = fs::read(installer)?;
    let offset = archive_offset(&bytes)?;
    let extracted = temporary.path().join("sdk");
    fs::create_dir(&extracted)?;
    tar::Archive::new(GzDecoder::new(Cursor::new(&bytes[offset..]))).unpack(&extracted)?;
    let mut entries = Vec::new();
    files(&extracted, &mut entries)?;
    entries.sort();
    let header = entries
        .iter()
        .find(|p| p.file_name().is_some_and(|s| s == "Processing.NDI.Lib.h"))
        .ok_or("SDK headers missing")?;
    fs::create_dir_all(prefix.join("include"))?;
    fs::create_dir_all(prefix.join("lib"))?;
    for entry in fs::read_dir(header.parent().unwrap())? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_file()
            && name.starts_with("Processing.NDI.")
            && name.ends_with(".h")
        {
            fs::copy(entry.path(), prefix.join("include").join(name.as_ref()))?;
        }
    }
    let library = entries
        .iter()
        .find(|p| {
            library_matches(arch, p)
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("libndi.so.6."))
        })
        .ok_or("NDI v6 library for selected architecture missing")?;
    let filename = library.file_name().unwrap();
    fs::copy(library, prefix.join("lib").join(filename))?;
    for (name, target) in [
        ("libndi.so.6", filename.to_str().ok_or("library filename")?),
        ("libndi.so", "libndi.so.6"),
    ] {
        let dest = prefix.join("lib").join(name);
        if fs::symlink_metadata(&dest).is_ok() {
            fs::remove_file(&dest)?;
        }
        symlink(target, dest)?;
    }
    let version = filename
        .to_string_lossy()
        .trim_start_matches("libndi.so.")
        .to_owned();
    fs::write(prefix.join("ndimon-ndi-version"), format!("{version}\n"))?;
    fs::write(prefix.join("ndimon-ndi-sha256"), format!("{hash}\n"))?;
    println!("NDI SDK {version} ({arch}), archive SHA256 {hash}");
    Ok(())
}
pub fn sdk_task(args: &[String]) -> Result {
    match args {
        [flag, directory, arch_flag, arch]
            if flag == "--out"
                && arch_flag == "--arch"
                && matches!(arch.as_str(), "amd64" | "arm64" | "armhf") =>
        {
            sdk(Path::new(directory), arch)
        }
        _ => Err("ndi-sdk --out PREFIX --arch amd64|arm64|armhf".into()),
    }
}
fn install_sdk(root: &Path, arch: &str) -> Result {
    if Path::new("/usr/local/lib/libndi.so.6").exists()
        && Path::new("/usr/local/include/Processing.NDI.Lib.h").exists()
    {
        let library = fs::canonicalize("/usr/local/lib/libndi.so.6")?;
        let version = library
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("libndi.so.6."))
            .ok_or("existing SDK does not resolve to an identifiable v6 library")?;
        let record = root.join("dist/ndimon-ndi-version");
        fs::create_dir_all(record.parent().unwrap())?;
        fs::write(&record, format!("6.{version}\n"))?;
        sudo(
            root,
            "install",
            &[
                "-m",
                "0644",
                record.to_str().unwrap(),
                "/etc/ndimon-ndi-version",
            ],
        )?;
        println!("NDI SDK already installed; preserving existing v6 installation");
        return Ok(());
    }
    let stage = tempfile::tempdir()?;
    sdk(stage.path(), arch)?;
    sudo(
        root,
        "cp",
        &[
            "-a",
            stage.path().join("include/.").to_str().unwrap(),
            "/usr/local/include/",
        ],
    )?;
    sudo(
        root,
        "cp",
        &[
            "-a",
            stage.path().join("lib/.").to_str().unwrap(),
            "/usr/local/lib/",
        ],
    )?;
    for (source, dest) in [
        ("ndimon-ndi-version", "/etc/ndimon-ndi-version"),
        ("ndimon-ndi-sha256", "/etc/ndimon-ndi-sha256"),
    ] {
        sudo(
            root,
            "install",
            &[
                "-m",
                "0644",
                stage.path().join(source).to_str().unwrap(),
                dest,
            ],
        )?;
    }
    sudo(root, "ldconfig", &[])
}
fn mpp(root: &Path, board: &str, codename: &str) -> Result {
    if text("/sbin/ldconfig", &["-p"])?.contains("librockchip_mpp") {
        return Ok(());
    }
    let stage = tempfile::tempdir()?;
    let key = stage.path().join("keyring.deb");
    // Same known-good rotated archive keyring fallback as setup-deps.sh.
    download(
        "https://github.com/radxa-pkg/radxa-archive-keyring/releases/download/0.2.2/radxa-archive-keyring_0.2.2_all.deb",
        &key,
    )?;
    apt(root, &["install", "-y", key.to_str().unwrap()])?;
    let mut keys: Vec<_> = fs::read_dir("/usr/share/keyrings")?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("radxa-archive-keyring-") && n.ends_with(".gpg"))
        })
        .collect();
    keys.sort();
    let key = keys.last().ok_or("Radxa keyring missing")?;
    let sources = stage.path().join("radxa.list");
    let mut text = format!(
        "deb [signed-by={}] https://radxa-repo.github.io/bookworm bookworm main\ndeb [signed-by={}] https://radxa-repo.github.io/{board}-bookworm {board}-bookworm main\n",
        key.display(),
        key.display()
    );
    if codename == "noble" {
        for suite in ["noble", "noble-test"] {
            text.push_str(&format!(
                "deb [signed-by={}] https://radxa-repo.github.io/{suite} {suite} main\n",
                key.display()
            ));
        }
    }
    fs::write(&sources, text)?;
    sudo(
        root,
        "install",
        &[
            "-m",
            "0644",
            sources.to_str().unwrap(),
            "/etc/apt/sources.list.d/radxa-rockchip.list",
        ],
    )?;
    apt(root, &["update"])?;
    apt(
        root,
        &[
            "install",
            "-y",
            "--no-install-recommends",
            "librockchip-mpp1",
            "librockchip-mpp-dev",
            "librockchip-vpu0",
        ],
    )
}
fn ffmpeg7(root: &Path, arch: &str) -> Result {
    let multiarch = match arch {
        "amd64" => "x86_64-linux-gnu",
        "arm64" => "aarch64-linux-gnu",
        "armhf" => "arm-linux-gnueabihf",
        _ => return Err("unsupported FFmpeg architecture".into()),
    };
    // Remove only the historical, ABI-incompatible FFmpeg 6 shims. Real
    // FFmpeg 7 package symlinks must remain intact.
    for base in ["/lib", "/usr/lib"] {
        for (name, stale) in [
            ("libavcodec.so.61", "libavcodec.so.60"),
            ("libavutil.so.59", "libavutil.so.58"),
        ] {
            let link = Path::new(base).join(multiarch).join(name);
            if fs::read_link(&link)
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .is_some_and(|name| name.starts_with(stale))
            {
                sudo(root, "rm", &["--", link.to_str().unwrap()])?;
            }
        }
    }
    if text("/sbin/ldconfig", &["-p"])?.contains("libavcodec.so.61") {
        return Ok(());
    }
    // Prefer the distro's real ABI packages. Never create cross-ABI symlinks.
    if apt(
        root,
        &[
            "install",
            "-y",
            "--no-install-recommends",
            "libavcodec61",
            "libavutil59",
        ],
    )
    .is_ok()
    {
        return Ok(());
    }
    let stage = tempfile::tempdir()?;
    let key = stage.path().join("ubuntu.asc");
    download(
        "https://keyserver.ubuntu.com/pks/lookup?op=get&search=0x871920D1991BC93C",
        &key,
    )?;
    checked(
        root,
        "gpg",
        &[
            "--batch",
            "--dearmor",
            "--output",
            stage.path().join("ubuntu.gpg").to_str().unwrap(),
            key.to_str().unwrap(),
        ],
    )?;
    sudo(
        root,
        "install",
        &[
            "-m",
            "0644",
            stage.path().join("ubuntu.gpg").to_str().unwrap(),
            "/usr/share/keyrings/ubuntu-archive-ndimon.gpg",
        ],
    )?;
    let list = stage.path().join("ffmpeg.list");
    let pin = stage.path().join("ffmpeg.pin");
    let mirror = if arch == "amd64" {
        "https://archive.ubuntu.com/ubuntu"
    } else {
        "https://ports.ubuntu.com/ubuntu-ports"
    };
    fs::write(&list, ffmpeg_source(arch, mirror))?;
    fs::write(&pin, FFMPEG_PIN_TEXT)?;
    // What the build compiles against, before and after.
    let dev_before = installed_versions(FFMPEG_DEV)?;
    sudo(
        root,
        "install",
        &["-m", "0644", pin.to_str().unwrap(), FFMPEG_PIN],
    )?;
    sudo(
        root,
        "install",
        &["-m", "0644", list.to_str().unwrap(), FFMPEG_SOURCE],
    )?;
    let result = (|| {
        apt(root, &["update"])?;
        apt(
            root,
            &[
                "install",
                "-y",
                "--no-install-recommends",
                "libavcodec61",
                "libavutil59",
            ],
        )
    })();
    forget_foreign_ffmpeg(root)?;
    let _ = apt(root, &["update"]);
    // The pin should have kept noble's own packages; make sure, and undo if not.
    let dev_after = installed_versions(FFMPEG_DEV)?;
    if dev_after != dev_before {
        let mut restore = vec!["install", "-y", "--allow-downgrades"];
        let pinned: Vec<String> = dev_before.iter().map(|(n, v)| format!("{n}={v}")).collect();
        restore.extend(pinned.iter().map(String::as_str));
        let _ = apt(root, &restore);
        return Err(
            "the FFmpeg 7 runtime changed the FFmpeg development packages; restored".into(),
        );
    }
    result
}
/// The temporary source and its pin, if a previous run left them (an interrupted
/// run must not let the next `apt-get install` pick plucky's `-dev` packages).
fn forget_foreign_ffmpeg(root: &Path) -> Result {
    sudo(root, "rm", &["-f", "--", FFMPEG_SOURCE, FFMPEG_PIN])
}
fn ffmpeg_source(arch: &str, mirror: &str) -> String {
    format!(
        "deb [arch={arch} signed-by=/usr/share/keyrings/ubuntu-archive-ndimon.gpg] {mirror} plucky main universe\n"
    )
}
/// `(package, version)` of the installed ones, in the order asked; not-installed
/// packages are left out.
fn installed_versions(packages: &[&str]) -> Result<Vec<(String, String)>> {
    let output = Command::new("dpkg-query")
        .args(["-W", "-f", "${Package} ${Status} ${Version}\n"])
        .args(packages)
        .output()?;
    // Exit status 1 only says a package is unknown; the others are still listed.
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once(' ')?;
            let (status, version) = rest.rsplit_once(' ')?;
            status
                .ends_with(" installed")
                .then(|| (name.into(), version.into()))
        })
        .collect())
}
pub fn setup(root: &Path, args: &[String]) -> Result {
    if !args.is_empty() {
        return Err("setup-deps accepts no options; NDI_SDK_SHA256 pins the SDK download".into());
    }
    let arch = text("dpkg", &["--print-architecture"])?;
    let compatible = fs::read_to_string("/proc/device-tree/compatible").unwrap_or_default();
    let os = fs::read_to_string("/etc/os-release")?;
    let codename = os
        .lines()
        .find_map(|l| l.strip_prefix("VERSION_CODENAME="))
        .unwrap_or_default()
        .trim_matches('"');
    // A run that was killed between adding the plucky source and removing it
    // would let this `apt-get install` pick plucky's FFmpeg development packages.
    forget_foreign_ffmpeg(root)?;
    apt(root, &["update"])?;
    let mut packages = vec!["install", "-y", "--no-install-recommends"];
    packages.extend(BUILD_DEPS);
    apt(root, &packages)?;
    for dependency in OPTIONAL_BUILD_DEPS {
        if let Err(e) = apt(
            root,
            &["install", "-y", "--no-install-recommends", dependency],
        ) {
            eprintln!("{dependency} unavailable; building without it (bundled zlib): {e}");
        }
    }
    for board in ["rk3588", "rk3399"] {
        if compatible.contains(board) {
            if let Err(e) = mpp(root, board, codename) {
                eprintln!("MPP unavailable; software decode fallback: {e}");
            }
        }
    }
    // Pi V4L2 uses the kernel interface and remains enabled by CMake. NVIDIA
    // skips VAAPI at runtime in PlatformDetect, preserving software fallback.
    if arch == "amd64" {
        apt(
            root,
            &[
                "install",
                "-y",
                "--no-install-recommends",
                "libva2",
                "libva-drm2",
                "libva-dev",
                "vainfo",
                "mesa-va-drivers",
            ],
        )?;
        let _ = apt(
            root,
            &[
                "install",
                "-y",
                "--no-install-recommends",
                "intel-media-va-driver-non-free",
                "i965-va-driver",
            ],
        );
        if let Ok(out) = Command::new("vainfo").args(["--display", "drm"]).output() {
            let info = root.join("dist/vainfo");
            fs::create_dir_all(info.parent().unwrap())?;
            let mut text = out.stdout;
            text.extend(out.stderr);
            fs::write(&info, text)?;
            sudo(
                root,
                "install",
                &[
                    "-m",
                    "0644",
                    info.to_str().unwrap(),
                    "/etc/ndimon-vaapi-info",
                ],
            )?;
        }
    }
    install_sdk(root, &arch)?;
    if let Err(e) = ffmpeg7(root, &arch) {
        eprintln!("FFmpeg 7 unavailable; NDI HX H.265 cannot decode: {e}");
    }
    if codename == "noble" {
        println!(
            "noble: the build and ndimon-runtime use noble's FFmpeg 6.1 (libavcodec60); only the \
             runtime libraries libavcodec61 and libavutil59 may come from plucky, for the NDI \
             SDK's HX decoder"
        );
    }
    Ok(())
}
/// apt's arguments for installing the package set. Downgrades are allowed, and
/// an operator's edit of a conffile (the list of certified network backends) is
/// neither replaced nor asked about: dpkg keeps the old file whenever both
/// sides changed it.
fn install_arguments(packages: &[PathBuf]) -> Result<Vec<&str>> {
    let mut argv = vec![
        "install",
        "-y",
        "--allow-downgrades",
        "-o",
        "Dpkg::Options::=--force-confdef",
        "-o",
        "Dpkg::Options::=--force-confold",
    ];
    for path in packages {
        argv.push(path.to_str().ok_or("package path")?);
    }
    Ok(argv)
}
pub fn run(root: &Path, args: &[String]) -> Result {
    let mut no_deps = false;
    let mut package_args = Vec::new();
    for arg in args {
        if arg == "--no-deps" {
            no_deps = true;
        } else {
            package_args.push(arg.clone());
        }
    }
    if !no_deps {
        setup(root, &[])?;
    }
    let packages = package::build(root, &package_args)?;
    apt(root, &install_arguments(&packages)?)?;
    println!(
        "Installed native control plane. Open http://<device-ip>/; see journalctl -u ndimon-control."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sdk_marker_requires_a_line_and_preserves_binary_archive_bytes() {
        let data = b"#!/bin/sh\n__NDI_ARCHIVE_BEGIN__\n\x1f\x8b\0";
        assert_eq!(&data[archive_offset(data).unwrap()..], b"\x1f\x8b\0");
        assert!(archive_offset(b"not an installer").is_err());
        assert!(archive_offset(b"echo __NDI_ARCHIVE_BEGIN__\n").is_err());
    }
    #[test]
    fn a_device_that_builds_its_own_packages_gets_no_node() {
        // The committed UI bundle ships; the pinned vendor files come from
        // `vendor::build`. Node is a developer and CI tool only.
        for dependency in BUILD_DEPS {
            assert!(
                !dependency.starts_with("node") && !dependency.starts_with("npm"),
                "{dependency}"
            );
        }
    }
    #[test]
    fn installing_the_package_set_keeps_an_operator_edited_conffile() {
        let packages = [
            PathBuf::from("dist/debian/ndimon-privd_1.4.0-1_amd64.deb"),
            PathBuf::from("dist/debian/ndimon-ui_1.4.0-1_amd64.deb"),
        ];
        let argv = install_arguments(&packages).unwrap();
        // Without these, dpkg asks about a conffile that both the operator and the
        // package changed, and fails where nobody can answer.
        assert!(argv.contains(&"Dpkg::Options::=--force-confdef"));
        assert!(argv.contains(&"Dpkg::Options::=--force-confold"));
        assert_eq!(argv[0], "install");
        assert_eq!(
            &argv[argv.len() - 2..],
            [packages[0].to_str().unwrap(), packages[1].to_str().unwrap()]
        );
    }
    #[test]
    fn zlib_headers_are_optional_and_plucky_stays_below_the_distribution() {
        // HW-1 H9: an uninstallable zlib1g-dev stopped the whole dependency install.
        assert!(!BUILD_DEPS.contains(&"zlib1g-dev"));
        assert_eq!(OPTIONAL_BUILD_DEPS, ["zlib1g-dev"]);
        // The pin is below the default priority (500) of the installed distribution,
        // so only a package noble lacks (libavcodec61) can come from plucky.
        assert!(FFMPEG_PIN_TEXT.contains("Pin: release n=plucky"));
        assert!(FFMPEG_PIN_TEXT.contains("Pin-Priority: 100"));
        assert!(FFMPEG_PIN_TEXT.starts_with("Package: *\n"));
        let source = ffmpeg_source("arm64", "https://ports.ubuntu.com/ubuntu-ports");
        assert_eq!(
            source,
            "deb [arch=arm64 signed-by=/usr/share/keyrings/ubuntu-archive-ndimon.gpg] https://ports.ubuntu.com/ubuntu-ports plucky main universe\n"
        );
        // Both files vanish together.
        assert!(FFMPEG_SOURCE.contains("ndimon-ffmpeg-temporary"));
        assert!(FFMPEG_PIN.contains("ndimon-ffmpeg-temporary"));
        for package in FFMPEG_DEV {
            assert!(BUILD_DEPS.contains(package), "{package}");
        }
    }
    #[test]
    fn sdk_library_selection_never_crosses_architectures() {
        for (arch, directory) in [
            ("amd64", "x86_64-linux-gnu"),
            ("arm64", "aarch64-rpi4-linux-gnueabi"),
            ("armhf", "arm-rpi3-linux-gnueabihf"),
        ] {
            for target in ["amd64", "arm64", "armhf"] {
                assert_eq!(
                    library_matches(target, Path::new(directory)),
                    arch == target
                );
            }
        }
    }
}
