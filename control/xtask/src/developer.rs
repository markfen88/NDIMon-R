//! Developer entry points. Every subprocess receives arguments directly.
use crate::{Result, checked};
use std::{env, path::Path};

pub fn build(root: &Path, args: &[String]) -> Result {
    let mut configure = vec![
        "-S",
        ".",
        "-B",
        "build",
        "-DCMAKE_BUILD_TYPE=Release",
        "-DCMAKE_INSTALL_PREFIX=/usr",
    ];
    configure.extend(args.iter().map(String::as_str));
    checked(root, "cmake", &configure)?;
    checked(root, "cmake", &["--build", "build", "--parallel", &jobs()])
}

fn jobs() -> String {
    env::var("CMAKE_BUILD_PARALLEL_LEVEL").unwrap_or_else(|_| "2".into())
}

fn switches(args: &[String], alsa_env: bool, ipc_env: bool) -> Result<(bool, bool)> {
    let (mut alsa, mut ipc) = (alsa_env, ipc_env);
    for arg in args {
        match arg.as_str() {
            "--alsa-loopback" => alsa = true,
            "--ipc-tests" => ipc = true,
            _ => return Err(format!("unknown unit-tests option: {arg}").into()),
        }
    }
    Ok((alsa, ipc))
}

pub fn unit_tests(root: &Path, args: &[String]) -> Result {
    let (alsa, ipc) = switches(
        args,
        env::var("RUN_ALSA_LOOPBACK").as_deref() == Ok("1"),
        env::var("RUN_IPC_TESTS").as_deref() == Ok("1"),
    )?;
    let mut configure = vec![
        "-S",
        "tests",
        "-B",
        "build/unit",
        "-DCMAKE_BUILD_TYPE=Release",
        if alsa {
            "-DNDIMON_ALSA_LOOPBACK=ON"
        } else {
            "-DNDIMON_ALSA_LOOPBACK=OFF"
        },
        if ipc {
            "-DNDIMON_IPC_TESTS=ON"
        } else {
            "-DNDIMON_IPC_TESTS=OFF"
        },
    ];
    let json = env::var("NDIMON_JSON_INCLUDE").ok();
    let include = json
        .as_ref()
        .map(|path| format!("-DCMAKE_CXX_FLAGS=-I{path}"));
    if let Some(include) = &include {
        configure.push(include);
    }
    checked(root, "cmake", &configure)?;
    checked(
        root,
        "cmake",
        &["--build", "build/unit", "--parallel", &jobs()],
    )?;
    checked(
        root,
        "ctest",
        &[
            "--test-dir",
            "build/unit",
            "--output-on-failure",
            "--parallel",
            &jobs(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guest_flags_preserve_environment_switches_and_reject_unknown_options() {
        assert_eq!(switches(&[], false, true).unwrap(), (false, true));
        assert_eq!(
            switches(
                &["--alsa-loopback".into(), "--ipc-tests".into()],
                false,
                false
            )
            .unwrap(),
            (true, true)
        );
        assert!(switches(&["--shell".into()], false, false).is_err());
    }
}
