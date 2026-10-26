//! Build-host tasks. Never invokes a shell or installs a toolchain.
mod developer;
mod install;
mod net_harness;
mod package;
mod plugin;
mod release;
mod vendor;

use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

fn main() {
    if let Err(error) = run() {
        eprintln!("xtask: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("workspace missing")?
        .to_path_buf();
    let root = workspace.parent().ok_or("repository missing")?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("ci") if args.len() == 1 => {
            for args in [
                vec!["fmt", "--all", "--check"],
                vec![
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--all-features",
                    "--",
                    "-D",
                    "warnings",
                ],
                vec!["test", "--workspace", "--all-features"],
                vec!["check", "-p", "ndimon-types", "--no-default-features"],
            ] {
                checked(&workspace, "cargo", &args)?;
            }
            let output = Command::new("cargo")
                .arg("--list")
                .current_dir(&workspace)
                .output()?;
            if !output.status.success() {
                return Err("cargo --list failed".into());
            }
            if String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.split_whitespace().next() == Some("deny"))
            {
                checked(&workspace, "cargo", &["deny", "check"])?;
            } else {
                eprintln!("SKIP cargo deny check: cargo-deny is not installed");
            }
            check_no_scripts(root)?;
        }
        Some("export-types") if args.len() == 1 => export_types(root)?,
        Some("build") => developer::build(root, &args[1..])?,
        Some("unit-tests") => developer::unit_tests(root, &args[1..])?,
        Some("build-ui") if args.len() == 1 => {
            let ui = root.join("ui");
            checked(&ui, "npm", &["ci"])?;
            checked(&ui, "npm", &["run", "build"])?;
            vendor::build(root)?;
        }
        Some("build-vendor") if args.len() == 1 => vendor::build(root)?,
        Some("check-ui") if args.len() == 1 => {
            checked(&root.join("ui"), "npm", &["run", "check"])?;
        }
        Some("check-no-scripts") if args.len() == 1 => check_no_scripts(root)?,
        Some("build-plugin") => { plugin::build(root, &args[1..])?; }
        Some("release") => release::release(&args[1..])?,
        Some("release-smoke") => release::smoke(&args[1..])?,
        Some("verify-release") => release::verify(&args[1..])?,
        Some("release-keygen") => release::keygen(&args[1..])?,
        Some("net-harness") => net_harness::run(&workspace, &args[1..])?,
        Some("build-deb") => { package::build(root, &args[1..])?; }
        Some("install") => install::run(root, &args[1..])?,
        Some("setup-deps") => install::setup(root, &args[1..])?,
        Some("ndi-sdk") => install::sdk_task(&args[1..])?,
        _ => return Err("usage: cargo xtask <ci|build|unit-tests|export-types|build-ui|build-vendor|check-ui|check-no-scripts|build-plugin|build-deb|install|setup-deps|ndi-sdk|release|verify-release|release-keygen|net-harness>".into()),
    }
    Ok(())
}

fn checked(directory: &Path, executable: &str, args: &[&str]) -> Result {
    let status = Command::new(executable)
        .args(args)
        .current_dir(directory)
        .status()?;
    if !status.success() {
        return Err(format!("{executable} {} failed: {status}", args.join(" ")).into());
    }
    Ok(())
}

fn export_types(root: &Path) -> Result {
    use ndimon_types::*;
    use ts_rs::TS;
    let ts = root.join("ui/src/generated");
    let schema = root.join("control/schema");
    fs::create_dir_all(&ts)?;
    fs::create_dir_all(&schema)?;
    macro_rules! export {
        ($ty:ty) => {{
            <$ty>::export_all_to(&ts)?;
            let value = schemars::schema_for!($ty);
            fs::write(
                schema.join(concat!(stringify!($ty), ".json")),
                format!("{}\n", serde_json::to_string_pretty(&value)?),
            )?;
        }};
    }
    export!(DesiredState);
    export!(PluginBackupSection);
    export!(PluginBackupExport);
    export!(NetworkRequest);
    export!(CommandRequest);
    export!(CommandReply);
    export!(Origin);
    export!(Event);
    export!(Query);
    export!(ErrorEnvelope);
    export!(Observed);
    export!(SessionCommand);
    Ok(())
}

/// Inspect tracked and untracked source files. Exclude only VCS/dependency/build
/// trees, not arbitrary ignored paths: an ignored new script must still fail.
fn source_files(root: &Path) -> Result<Vec<PathBuf>> {
    fn visit(root: &Path, path: &Path, files: &mut Vec<PathBuf>) -> Result {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path.strip_prefix(root)?;
            let name = entry.file_name();
            let ignored = matches!(
                name.to_str(),
                Some(".git" | "node_modules" | "__pycache__" | ".pytest_cache")
            ) || [
                "build",
                "dist",
                "control/target",
                ".venv",
                "venv",
                "reports",
                ".superpowers",
            ]
            .iter()
            .any(|excluded| relative == Path::new(excluded));
            if ignored {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                visit(root, &path, files)?;
            } else if kind.is_file() || kind.is_symlink() {
                files.push(relative.to_path_buf());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

/// Shell or Python, by extension or by interpreter line (also without one).
fn is_script(root: &Path, path: &Path) -> Result<bool> {
    if matches!(
        path.extension().and_then(OsStr::to_str),
        Some("py" | "sh" | "bash" | "zsh" | "ksh")
    ) {
        return Ok(true);
    }
    // Inspect in-tree symlink targets as well; never follow one outside this
    // checkout while searching for extensionless shell scripts.
    let resolved;
    let path = if fs::symlink_metadata(path)?.file_type().is_symlink() {
        resolved = fs::canonicalize(path)?;
        if !resolved.starts_with(root) {
            return Err("script guard refuses an external source symlink".into());
        }
        resolved.as_path()
    } else {
        path
    };
    let mut prefix = [0; 256];
    let len = fs::File::open(path)?.read(&mut prefix)?;
    let line = String::from_utf8_lossy(&prefix[..len]);
    let line = line.lines().next().unwrap_or("");
    if !line.starts_with("#!") {
        return Ok(false);
    }
    Ok(line[2..].split_whitespace().any(|word| {
        let program = word.rsplit('/').next().unwrap_or("");
        matches!(program, "sh" | "bash" | "dash" | "ash" | "ksh" | "zsh") || is_python(program)
    }))
}

/// `python`, `python3`, `python3.13` and the like.
fn is_python(program: &str) -> bool {
    program
        .strip_prefix("python")
        .is_some_and(|rest| rest.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
}

/// The repository holds no shell and no Python. There is no allowlist.
fn check_no_scripts(root: &Path) -> Result {
    let mut found = Vec::new();
    for relative in source_files(root)? {
        if is_script(root, &root.join(&relative))? {
            found.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    if !found.is_empty() {
        return Err(format!(
            "shell and Python scripts are forbidden:\n{}",
            found.join("\n")
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_is_a_hard_ban_on_extensions_and_extensionless_shell_and_python() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        check_no_scripts(root).unwrap();
        // A leftover allowlist file does not excuse anything.
        fs::create_dir_all(root.join("control/xtask")).unwrap();
        fs::write(root.join("control/xtask/legacy-scripts.txt"), "old.sh\n").unwrap();
        fs::write(root.join("old.sh"), "legacy").unwrap();
        assert!(check_no_scripts(root).is_err());
        fs::remove_file(root.join("old.sh")).unwrap();
        for (name, content) in [
            ("new.py", "text"),
            ("new.sh", "text"),
            ("new.bash", "text"),
            ("helper", "#!/usr/bin/env -S bash -eu\n"),
            ("helper", "#!/bin/dash\n"),
            ("tool", "#!/usr/bin/python3\n"),
            ("tool", "#!/usr/bin/python\nimport os\n"),
            ("tool", "#!/usr/bin/env python3\n"),
            ("tool", "#!/usr/bin/env -S python3 -u\n"),
            ("tool", "#! /usr/local/bin/python3.13\n"),
        ] {
            fs::write(root.join(name), content).unwrap();
            assert!(check_no_scripts(root).is_err(), "{name}: {content}");
            fs::remove_file(root.join(name)).unwrap();
        }
        // Source that merely mentions an interpreter, or runs on Node, is fine.
        fs::write(root.join("text.rs"), "// #!/bin/sh\n").unwrap();
        fs::write(
            root.join("page.ts"),
            "#!/usr/bin/env -S node --no-warnings\n",
        )
        .unwrap();
        fs::write(root.join("notes"), "python is not a shebang here\n").unwrap();
        fs::write(root.join("pythonista"), "#!/usr/bin/env pythonista\n").unwrap();
        check_no_scripts(root).unwrap();
    }
}
