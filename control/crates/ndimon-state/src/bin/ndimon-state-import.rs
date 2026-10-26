//! Run once as root, ordered before ndimon-control/decoder start. Never
//! run concurrently with the actor. The binary reads only the declared inputs.
use ndimon_state::{
    domains::display::assets::AssetStore, import, observed_clock, persist::Persistence,
};
use nix::unistd::{User, chown, geteuid};
use std::{os::unix::fs::PermissionsExt, path::Path};

fn main() {
    if let Err(e) = run() {
        eprintln!("state migration: {e}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), String> {
    if !geteuid().is_root() {
        return Err("root migration must run before the control service starts".into());
    }
    if std::env::args_os().len() != 1 {
        return Err("usage: ndimon-state-import (no arguments)".into());
    }
    let user = User::from_name("ndimon-control")
        .map_err(|e| e.to_string())?
        .ok_or("ndimon-control user is missing")?;
    let store = Persistence::new("/var/lib/ndimon/state")?;
    if store.load()?.is_none() {
        let files = import::read_files(Path::new("/"), true)?;
        let (mut state, mut skipped) = import::import_tolerant(files)?;
        // HW-1 H4: the running daemon's clock role and priority, not a default.
        if let Err(e) = observed_clock::keep(
            &mut state,
            observed_clock::observe(Path::new(observed_clock::SOCKET)),
        ) {
            skipped.push(import::NotImported {
                domain: "tessera clock role and priority",
                reason: e,
            });
        }
        store.initialize(&state)?;
        // The old stack is stopped for this migration: what the importer refuses
        // costs its domain, never the device. Say which, and where the original is.
        for item in &skipped {
            eprintln!(
                "state migration: NOT IMPORTED, left at its defaults: {item} (the original legacy files are kept as <file>.pre-control)"
            );
        }
    }
    let assets = AssetStore::new("/var/lib/ndimon/assets");
    std::fs::create_dir_all(&assets.directory).map_err(|e| e.to_string())?;
    if std::fs::symlink_metadata(&assets.directory)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("linked asset directory".into());
    }
    std::fs::set_permissions(&assets.directory, std::fs::Permissions::from_mode(0o750))
        .map_err(|e| e.to_string())?;
    if !store.directory.join("display-assets-v1").exists() {
        let mut state = store.load()?.ok_or("state missing")?;
        // A configured logo that is not a plain, small image costs the logo.
        if let Err(reason) = assets.import_legacy(&mut state, Path::new("/")) {
            eprintln!(
                "state migration: NOT IMPORTED, left at its defaults: display.splash.logo: {reason}"
            );
        }
        store.initialize(&state)?;
        store.write("display-assets-v1", b"1")?;
    }
    for entry in std::fs::read_dir(&assets.directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            return Err("nonregular managed asset".into());
        }
        chown(&entry.path(), Some(user.uid), Some(user.gid)).map_err(|e| e.to_string())?;
    }
    chown(&assets.directory, Some(user.uid), Some(user.gid)).map_err(|e| e.to_string())?;
    // The control service is stopped during this migration. Runtime writes use
    // descriptor-pinned privd destinations and the actor's private state dir.
    for name in [
        "state.json",
        "state.prev.json",
        "rendered.json",
        "display-assets-v1",
    ] {
        let path = store.directory.join(name);
        if path.exists() {
            chown(&path, Some(user.uid), Some(user.gid)).map_err(|e| e.to_string())?;
        }
    }
    chown(&store.directory, Some(user.uid), Some(user.gid)).map_err(|e| e.to_string())?;
    Ok(())
}
