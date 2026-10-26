//! One package builder for CI and appliance builds. dpkg-deb preserves native
//! ELF maintainer hooks rather than generating shell hooks through debhelper.
use crate::{Result, checked};
use std::{
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

pub const PACKAGES: &[&str] = &[
    "ndimon-privd",
    "ndimon-control",
    "ndimon-ui",
    "ndimon-runtime",
];
/// The retired Node follower package. ndimon-control replaces it so that an
/// upgrade from an intermediate build removes it instead of keeping its unit.
const RETIRED_PACKAGE: &str = "ndimon-legacy-api";
pub const DEFAULT_VERSION: &str = "1.4.0-1";
/// Files below /etc that an operator may edit and an upgrade must keep, as
/// (path in the package, source in the repository). dpkg treats them as
/// conffiles: a fresh install and an upgrade from a 1.3.x install both create a
/// missing one, and an edited or deleted one is never overwritten or restored.
const PRIVD_CONFFILES: &[(&str, &str)] = &[(
    "etc/ndimon-network-certified-backends.json",
    "packaging/debian/etc/ndimon-network-certified-backends.json",
)];

fn required_assets(root: &Path) -> Result {
    for asset in [
        "api/public/ui/api.js",
        "api/public/ui/store.js",
        "api/public/ui/csrf.js",
        "api/public/ui/page.js",
        "api/public/network-settings.js",
        "api/public/vendor/bootstrap/css/bootstrap.min.css",
        "api/public/vendor/bootstrap/js/bootstrap.bundle.min.js",
        "api/public/vendor/bootstrap/LICENSE",
        "api/public/vendor/bootstrap-icons/font/bootstrap-icons.min.css",
        "api/public/vendor/bootstrap-icons/font/fonts/bootstrap-icons.woff2",
        "api/public/vendor/bootstrap-icons/LICENSE",
        "plugins/tessera/plugin.json",
        "plugins/tessera/ui.html",
    ] {
        if !root.join(asset).is_file() {
            return Err(format!(
                "required package asset missing: {asset}; run cargo xtask build-vendor (vendor files) or build-ui (page bundle)"
            )
            .into());
        }
    }
    Ok(())
}

fn copy(source: &Path, dest: &Path, mode: u32) -> Result {
    fs::create_dir_all(dest.parent().ok_or("asset parent")?)?;
    fs::copy(source, dest)?;
    fs::set_permissions(dest, fs::Permissions::from_mode(mode))?;
    Ok(())
}
/// Stage the conffiles and list them in `DEBIAN/conffiles` (absolute paths).
fn conffiles(root: &Path, stage: &Path, files: &[(&str, &str)]) -> Result {
    let mut list = String::new();
    for (path, source) in files {
        copy(&root.join(source), &stage.join(path), 0o644)?;
        list.push_str(&format!("/{path}\n"));
    }
    fs::create_dir_all(stage.join("DEBIAN"))?;
    fs::write(stage.join("DEBIAN/conffiles"), list)?;
    Ok(())
}
/// Inspect a built ndimon-privd package: it lists its conffiles and holds each.
pub fn verify_privd_conffiles(package: &Path) -> Result {
    let text = |flag: &str, member: Option<&str>| -> Result<String> {
        let output = Command::new("dpkg-deb")
            .arg(flag)
            .arg(package)
            .args(member)
            .output()?;
        if !output.status.success() {
            return Err(format!("dpkg-deb {flag} {}: {}", package.display(), output.status).into());
        }
        Ok(String::from_utf8(output.stdout)?)
    };
    let listed = text("-I", Some("conffiles"))?;
    let contents = text("-c", None)?;
    for (path, _) in PRIVD_CONFFILES {
        if !listed.lines().any(|line| line == format!("/{path}")) {
            return Err(format!("{}: /{path} is not a conffile", package.display()).into());
        }
        if !contents
            .lines()
            .any(|line| line.ends_with(&format!(" ./{path}")))
        {
            return Err(format!("{}: /{path} is not in the payload", package.display()).into());
        }
    }
    Ok(())
}
fn tree(source: &Path, dest: &Path) -> Result {
    fs::create_dir_all(dest)?;
    fs::set_permissions(dest, fs::Permissions::from_mode(0o755))?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if matches!(
            name.to_str(),
            Some(
                ".bin"
                    | ".package-lock.json"
                    | "test"
                    | "tests"
                    | "__tests__"
                    | ".github"
                    | ".gitattributes"
                    | ".gitignore"
            )
        ) {
            continue;
        }
        let target = dest.join(&name);
        let kind = entry.file_type()?;
        if kind.is_dir() {
            tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            let mode = if entry.metadata()?.permissions().mode() & 0o111 != 0 {
                0o755
            } else {
                0o644
            };
            copy(&entry.path(), &target, mode)?;
        } else {
            return Err(format!("unsupported package asset: {}", entry.path().display()).into());
        }
    }
    Ok(())
}
/// Scan actual staged bytes, including extensionless maintainer hooks. A
/// package contains no shell, Python or Node program and no Node dependency tree.
pub fn audit(directory: &Path) -> Result {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if entry.file_name() == "node_modules" {
                return Err(format!(
                    "Node dependency tree in package: {}",
                    entry.path().display()
                )
                .into());
            }
            audit(&entry.path())?;
            continue;
        }
        if !entry.file_type()?.is_file() {
            return Err("package contains a nonregular asset".into());
        }
        let file = entry.path();
        if matches!(
            file.extension().and_then(|s| s.to_str()),
            Some("sh" | "py" | "pyc")
        ) {
            return Err(format!("legacy script in package: {}", file.display()).into());
        }
        let mut prefix = [0; 256];
        let n = fs::File::open(&file)?.read(&mut prefix)?;
        if prefix[..n].starts_with(b"#!") {
            let line = String::from_utf8_lossy(&prefix[..n]);
            if line
                .lines()
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .any(|s| {
                    let s = s
                        .trim_start_matches("#!")
                        .rsplit('/')
                        .next()
                        .unwrap_or_default();
                    matches!(
                        s,
                        "sh" | "bash" | "dash" | "ash" | "ksh" | "zsh" | "node" | "nodejs"
                    ) || s.starts_with("python")
                })
            {
                return Err(format!(
                    "shell/Python/Node interpreter in package: {}",
                    file.display()
                )
                .into());
            }
        }
    }
    Ok(())
}
/// No package may depend on Node, even optionally: it is a build tool only.
pub fn audit_control(stage: &Path) -> Result {
    let control = fs::read_to_string(stage.join("DEBIAN/control"))?;
    for line in control.lines() {
        let Some((field, value)) = line.split_once(':') else {
            continue;
        };
        if !matches!(field, "Depends" | "Pre-Depends" | "Recommends" | "Suggests") {
            continue;
        }
        for dependency in value.split([',', '|']) {
            let name = dependency
                .trim()
                .split(|c: char| c.is_whitespace() || c == '(' || c == ':')
                .next()
                .unwrap_or_default();
            if matches!(name, "node" | "nodejs" | "npm") || name.starts_with("node-") {
                return Err(format!("{field} names Node ({name}) in {}", stage.display()).into());
            }
        }
    }
    Ok(())
}
/// The oldest systemd the units rely on: `Restart=on-failure` for a oneshot
/// service (ndimon-net-boot) arrived in 244.
const SYSTEMD: &str = "systemd (>= 244)";
/// What libndi.so needs and `dpkg-shlibdeps` cannot see, because the binaries
/// link it by file name (`-l` directory) and its own dependencies are not
/// theirs. Without these the decoder and finder exit 127 on a fresh install.
const NDI_LIBRARIES: &str = "libavahi-client3, libavahi-common3";
fn dependencies(name: &str, version: &str, shlibs: &str) -> String {
    match name {
        "ndimon-privd" => {
            format!("libc6, libgcc-s1, {SYSTEMD}, iproute2, nftables, iputils-arping, dbus")
        }
        "ndimon-control" => {
            format!("libc6, libgcc-s1, {SYSTEMD}, ndimon-privd (= {version})")
        }
        "ndimon-ui" => "libc6, libgcc-s1".into(),
        "ndimon-runtime" => format!(
            "{shlibs}, {NDI_LIBRARIES}, passwd, udev, avahi-daemon, ndimon-control (= {version}), ndimon-privd (= {version}), ndimon-ui (= {version})"
        ),
        _ => unreachable!(),
    }
}
fn native_dependencies(root: &Path) -> Result<String> {
    // shlibs discovery is performed against the built binaries on the target
    // distro. NDI remains a separately provisioned licensed dependency.
    let directory = root.join("dist/shlibs");
    fs::create_dir_all(directory.join("debian"))?;
    fs::write(
        directory.join("debian/control"),
        "Source: ndimon-r\nMaintainer: NDIMon-R maintainers <maintainers@ndimon.invalid>\n\nPackage: ndimon-runtime\nArchitecture: any\nDescription: NDIMon-R runtime\n",
    )?;
    let cache = fs::read_to_string(root.join("build/CMakeCache.txt"))?;
    let ndi = cache
        .lines()
        .find_map(|line| line.strip_prefix("NDI_LIB_DIR:PATH="))
        .ok_or("NDI library directory missing from CMake cache")?;
    let output = Command::new("dpkg-shlibdeps")
        .current_dir(&directory)
        .arg("-O")
        .arg(format!("-l{ndi}"))
        .arg("--ignore-missing-info")
        .arg(format!("-e{}", root.join("build/ndimon-r").display()))
        .arg(format!("-e{}", root.join("build/ndimon-finder").display()))
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "dpkg-shlibdeps: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let text = String::from_utf8(output.stdout)?;
    Ok(text
        .trim()
        .strip_prefix("shlibs:Depends=")
        .ok_or("missing shlibs dependencies")?
        .to_owned())
}
fn metadata(
    stage: &Path,
    root: &Path,
    name: &str,
    version: &str,
    arch: &str,
    depends: &str,
    binary: &Path,
) -> Result {
    let control = stage.join("DEBIAN");
    fs::create_dir_all(&control)?;
    let retired = if name == "ndimon-control" {
        format!("Conflicts: {RETIRED_PACKAGE}\nReplaces: {RETIRED_PACKAGE}\n")
    } else {
        String::new()
    };
    fs::write(
        control.join("control"),
        format!(
            "Package: {name}\nVersion: {version}\nArchitecture: {arch}\nMaintainer: NDIMon-R maintainers <maintainers@ndimon.invalid>\nSection: video\nPriority: optional\nDepends: {depends}\n{retired}Homepage: https://github.com/markfen88/NDIMon-R\nDescription: NDIMon-R {}\n Prebuilt appliance component with native package lifecycle hooks.\n",
            name.trim_start_matches("ndimon-")
        ),
    )?;
    for hook in ["preinst", "postinst", "prerm", "postrm"] {
        copy(binary, &control.join(hook), 0o755)?;
    }
    copy(
        &root.join("LICENSE"),
        &stage.join(format!("usr/share/doc/{name}/copyright")),
        0o644,
    )?;
    // A native hook is valid to dpkg although lintian's shell-only hook checks
    // do not understand ELF. Keep those policy deviations narrowly scoped.
    let override_path = stage.join(format!("usr/share/lintian/overrides/{name}"));
    fs::create_dir_all(override_path.parent().unwrap())?;
    fs::write(
        override_path,
        format!(
            "# Maintainer hooks are compiled Rust ELF programs, executed directly by dpkg.\n{name}: unstripped-binary-or-object *\n{name}: no-changelog *\n"
        ),
    )?;
    if name == "ndimon-privd" {
        let override_path = stage.join(format!("usr/share/lintian/overrides/{name}"));
        use std::io::Write;
        writeln!(
            fs::OpenOptions::new().append(true).open(override_path)?,
            "# yaml-rust2 is pure Rust; the libyaml string signature is a false positive.\n{name}: embedded-library libyaml [usr/libexec/ndimon/ndimon-privd]"
        )?;
    }
    Ok(())
}

pub fn build(root: &Path, args: &[String]) -> Result<Vec<PathBuf>> {
    let mut version = DEFAULT_VERSION.to_owned();
    let mut no_build = false;
    let mut options = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--version" => {
                i += 1;
                version = args.get(i).ok_or("--version needs a value")?.clone();
            }
            "--no-build" => no_build = true,
            "--" => {
                options.extend_from_slice(&args[i + 1..]);
                break;
            }
            _ => {
                return Err(
                    "build-deb [--version DEBIAN_VERSION] [--no-build] [-- CMAKE_OPTIONS...]"
                        .into(),
                );
            }
        }
        i += 1;
    }
    checked(root, "dpkg", &["--validate-version", &version])?;
    // Debian version must also be a flat filename component.
    if !version
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b".+:~-".contains(&c))
    {
        return Err("unsafe package version".into());
    }
    let arch = Command::new("dpkg").arg("--print-architecture").output()?;
    if !arch.status.success() {
        return Err("cannot detect package architecture".into());
    }
    let arch = String::from_utf8(arch.stdout)?.trim().to_owned();
    if !matches!(arch.as_str(), "amd64" | "arm64" | "armhf") {
        return Err("unsupported package architecture".into());
    }
    if !no_build {
        checked(
            &root.join("control"),
            "cargo",
            &["build", "--release", "--workspace"],
        )?;
        let mut cmake = vec!["-S", ".", "-B", "build", "-DCMAKE_BUILD_TYPE=Release"];
        cmake.extend(options.iter().map(String::as_str));
        checked(root, "cmake", &cmake)?;
        checked(root, "cmake", &["--build", "build", "--parallel", "2"])?;
        // The committed browser bundle is what ships; rebuilding it needs Node,
        // which only developers and CI have (`cargo xtask build-ui`). The pinned
        // vendor files are fetched and verified here, without npm.
        crate::vendor::build(root)?;
    }
    required_assets(root)?;
    let release = root.join("control/target/release");
    let hook_binary = release.join("ndimon-package");
    let output = root.join("dist/debian");
    fs::create_dir_all(&output)?;
    let shlibs = native_dependencies(root)?;
    let mut packages = Vec::new();
    for name in PACKAGES {
        let stage = root.join(format!("dist/staging/{name}"));
        if stage.exists() {
            fs::remove_dir_all(&stage)?;
        }
        fs::create_dir_all(&stage)?;
        let depend = dependencies(name, &version, &shlibs);
        metadata(&stage, root, name, &version, &arch, &depend, &hook_binary)?;
        match *name {
            "ndimon-control" => {
                for bin in [
                    "ndimon-control",
                    "ndimon-state-import",
                    "ndimonctl",
                    "ndimon-package",
                ] {
                    copy(
                        &release.join(bin),
                        &stage.join(format!("usr/bin/{bin}")),
                        0o755,
                    )?;
                }
                for unit in [
                    "ndimon-control.service",
                    "ndimon-control.socket",
                    "ndimon-state-import.service",
                ] {
                    copy(
                        &root.join("systemd").join(unit),
                        &stage.join(format!("usr/lib/systemd/system/{unit}")),
                        0o644,
                    )?;
                }
                for kind in ["sysusers.d", "tmpfiles.d"] {
                    copy(
                        &root.join(format!("packaging/debian/{kind}/ndimon.conf")),
                        &stage.join(format!("usr/lib/{kind}/ndimon.conf")),
                        0o644,
                    )?;
                }
                tree(
                    &root.join("config"),
                    &stage.join("usr/share/ndimon/defaults"),
                )?;
            }
            "ndimon-privd" => {
                copy(
                    &release.join("ndimon-privd"),
                    &stage.join("usr/libexec/ndimon/ndimon-privd"),
                    0o755,
                )?;
                for unit in [
                    "ndimon-privd.socket",
                    "ndimon-privd.service",
                    "ndimon-net-boot.service",
                ] {
                    copy(
                        &root.join("systemd").join(unit),
                        &stage.join(format!("usr/lib/systemd/system/{unit}")),
                        0o644,
                    )?;
                }
                copy(
                    &root.join("packaging/debian/licenses/webpki-roots.txt"),
                    &stage.join("usr/share/doc/ndimon-privd/webpki-roots.txt"),
                    0o644,
                )?;
                conffiles(root, &stage, PRIVD_CONFFILES)?;
            }
            "ndimon-ui" => {
                copy(
                    &root.join("api/public/vendor/bootstrap/LICENSE"),
                    &stage.join("usr/share/doc/ndimon-ui/copyright-bootstrap"),
                    0o644,
                )?;
                tree(
                    &root.join("api/public"),
                    &stage.join("usr/share/ndimon/public"),
                )?;
                // Tessera is first-party/native; only its manifest and UI are
                // needed by the native plugin asset route, never legacy hooks.
                for asset in ["plugin.json", "ui.html"] {
                    copy(
                        &root.join("plugins/tessera").join(asset),
                        &stage.join("usr/share/ndimon/plugins/tessera").join(asset),
                        0o644,
                    )?;
                }
            }
            "ndimon-runtime" => {
                copy(
                    &root.join("packaging/debian/udev/99-rockchip-rga.rules"),
                    &stage.join("usr/lib/udev/rules.d/99-rockchip-rga.rules"),
                    0o644,
                )?;
                for bin in ["ndimon-r", "ndimon-finder"] {
                    copy(
                        &root.join("build").join(bin),
                        &stage.join(format!("usr/bin/{bin}")),
                        0o755,
                    )?;
                }
                tree(&root.join("fonts"), &stage.join("usr/share/ndimon/fonts"))?;
                for unit in ["ndimon-r.service", "ndimon-finder.service"] {
                    copy(
                        &root.join("systemd").join(unit),
                        &stage.join(format!("usr/lib/systemd/system/{unit}")),
                        0o644,
                    )?;
                }
                let version_file = stage.join("usr/share/ndimon/version");
                fs::write(version_file, format!("{version}\n"))?;
                // What `/v1/System/version` reports as commit and build date
                // (HW-1 H7): the commit this package was built from and its date.
                fs::write(stage.join("usr/share/ndimon/build-info"), build_info(root))?;
            }
            _ => unreachable!(),
        }
        normalize_permissions(&stage)?;
        audit(&stage)?;
        audit_control(&stage)?;
        let file = output.join(format!("{name}_{version}_{arch}.deb"));
        checked(
            root,
            "dpkg-deb",
            &[
                "--root-owner-group",
                "--threads-max=2",
                "-Zxz",
                "--build",
                stage.to_str().ok_or("staging path")?,
                file.to_str().ok_or("package path")?,
            ],
        )?;
        packages.push(file);
    }
    Ok(packages)
}

/// `commit=` and `date=` of the checkout, deterministic for one commit. A tree
/// without git history says `unknown` instead of failing the build.
fn build_info(root: &Path) -> String {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".into())
    };
    format!(
        "commit={}\ndate={}\n",
        git(&["rev-parse", "--short=12", "HEAD"]),
        git(&["log", "-1", "--format=%cs"])
    )
}

fn normalize_permissions(directory: &Path) -> Result {
    fs::set_permissions(directory, fs::Permissions::from_mode(0o755))?;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            normalize_permissions(&entry.path())?;
        } else {
            let mode = if entry.metadata()?.permissions().mode() & 0o111 != 0 {
                0o755
            } else {
                0o644
            };
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_runtime_depends_on_what_libndi_needs_and_the_units_on_their_systemd() {
        let runtime = dependencies("ndimon-runtime", DEFAULT_VERSION, "libc6 (>= 2.41), libva2");
        let names: Vec<_> = runtime
            .split(',')
            .map(|d| d.split_whitespace().next().unwrap_or(""))
            .collect();
        // libndi.so.6 needs libavahi-client.so.3 and libavahi-common.so.3.
        for package in [
            "libavahi-client3",
            "libavahi-common3",
            "avahi-daemon",
            "libva2",
        ] {
            assert!(names.contains(&package), "{package} in {runtime}");
        }
        assert!(runtime.contains(&format!("ndimon-control (= {DEFAULT_VERSION})")));
        for package in PACKAGES {
            let depends = dependencies(package, DEFAULT_VERSION, "");
            assert_eq!(
                depends.contains(SYSTEMD),
                ["ndimon-privd", "ndimon-control"].contains(package),
                "{package}: {depends}"
            );
        }
    }
    #[test]
    fn payload_audit_checks_extensions_and_extensionless_interpreters() {
        let root = tempfile::tempdir().unwrap();
        for (name, data, good) in [
            ("hook", b"\x7fELF\0".as_slice(), true),
            ("script.py", b"text".as_slice(), false),
            ("script", b"#!/usr/bin/python3 -I\n".as_slice(), false),
            ("script", b"#!/usr/bin/env -S bash -eu\n".as_slice(), false),
            ("script", b"#!/bin/sh\n".as_slice(), false),
            ("script", b"#!/usr/bin/env node\n".as_slice(), false),
            ("script", b"#!/usr/bin/nodejs\n".as_slice(), false),
        ] {
            fs::write(root.path().join(name), data).unwrap();
            assert_eq!(audit(root.path()).is_ok(), good, "{name}");
            fs::remove_file(root.path().join(name)).unwrap();
        }
    }
    #[test]
    fn payload_audit_refuses_a_node_dependency_tree() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("usr/lib/x/node_modules/express")).unwrap();
        fs::write(
            root.path()
                .join("usr/lib/x/node_modules/express/index.html"),
            b"x",
        )
        .unwrap();
        assert!(audit(root.path()).is_err());
    }
    #[test]
    fn control_audit_refuses_node_dependencies_in_any_dependency_field() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("DEBIAN")).unwrap();
        for (text, good) in [
            ("Package: a\nDepends: libc6, systemd, udev | mdev\n", true),
            (
                "Package: a\nDepends: libc6 (>= 2.36), nodejs (>= 20)\n",
                false,
            ),
            ("Package: a\nRecommends: npm\n", false),
            ("Package: a\nSuggests: node-express:any\n", false),
            ("Package: a\nDepends: libc6 | nodejs\n", false),
            ("Package: a\nDescription: needs nodejs to build\n", true),
        ] {
            fs::write(root.path().join("DEBIAN/control"), text).unwrap();
            assert_eq!(audit_control(root.path()).is_ok(), good, "{text}");
        }
    }
    #[test]
    fn the_privd_package_ships_the_certification_list_as_an_operator_conffile() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let stage = tempfile::tempdir().unwrap();
        conffiles(&root, stage.path(), PRIVD_CONFFILES).unwrap();
        let listed = fs::read_to_string(stage.path().join("DEBIAN/conffiles")).unwrap();
        assert_eq!(listed, "/etc/ndimon-network-certified-backends.json\n");
        let file = stage
            .path()
            .join("etc/ndimon-network-certified-backends.json");
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o7777,
            0o644
        );
        // Every backend with real-tool acceptance in the nspawn harness (NE-11.1).
        let mut backends: Vec<String> = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        backends.sort();
        assert_eq!(
            backends,
            ["ifupdown", "netplan", "networkd", "networkmanager"]
        );
        // The payload audit accepts it, and every conffile is a file the package holds.
        audit(stage.path()).unwrap();
        for line in listed.lines() {
            assert!(stage.path().join(line.trim_start_matches('/')).is_file());
        }
    }
    /// The C++ project version, the operator documents and the default package
    /// version agree (this replaces the release-metadata check of the Node tests).
    #[test]
    fn the_release_version_is_named_consistently() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let read = |file: &str| fs::read_to_string(root.join(file)).unwrap();
        let cmake = read("CMakeLists.txt");
        let version = cmake
            .lines()
            .find_map(|line| line.strip_prefix("project(NDIMon-R VERSION "))
            .and_then(|rest| rest.split(' ').next())
            .expect("project version in CMakeLists.txt");
        assert!(
            read("README.md").contains(version),
            "README.md names {version}"
        );
        // `ndimonctl --version`, the crate metadata and the handoff documents the
        // network engine writes name the release too.
        assert_eq!(env!("CARGO_PKG_VERSION"), version);
        let numbers = |text: &str| -> Vec<u64> {
            text.split(['.', '-'])
                .map_while(|part| part.parse().ok())
                .collect()
        };
        assert!(numbers(DEFAULT_VERSION) >= numbers(version));
    }
}
