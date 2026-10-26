# NDIMon-R control-plane workspace

This Cargo workspace holds every program that runs on an NDIMon-R appliance
except the C++ decoder (`ndimon-r`) and finder (`ndimon-finder`), which live in
the repository's CMake project. There is no Node.js, Python or shell on the
device or in the repository: the browser UI is TypeScript built into committed
static files, and every build, packaging and release job is a `cargo xtask` task.

```text
browser (TypeScript UI) · scripts (HTTP API)
                │  HTTP + SSE
                ▼
        ndimon-control ── state actor: one desired-state document, one revision
        (user ndimon-control)   validate → plan impact → apply → persist → events
                │                     │ JSON-lines IPC        │ framed sockets
                │ typed requests      ▼                       ▼
                ▼               ndimon-r, ndimon-finder     tesserad (optional),
          ndimon-privd (root)   (C++ followers)             v2 plugin processes
          closed request enum, native network engine
```

`ndimon-control` is the only front door. Every change, from the UI, a script, a
plugin or boot, is a typed command sent to one state actor; nothing else writes
configuration. `ndimon-privd` is the only long-running root process besides the
decoder and accepts a closed set of typed requests: no path, shell text or
free-form arguments. The decoder and finder receive complete configuration domains
over their IPC sockets (`apply_config`) and keep their last configuration while the
control plane restarts after a failure.

## Crates

| Path | Purpose |
|---|---|
| [`crates/ndimon-types`](crates/ndimon-types/README.md) | Public contract: desired document, commands, queries, events, origins, impacts, errors and pure validation. Generates the TypeScript and JSON Schema |
| [`crates/ndimon-types/LEGACY-MAPPING.md`](crates/ndimon-types/LEGACY-MAPPING.md) | How each 1.3.x `/etc` setting is imported into the document and rendered back |
| [`crates/ndimon-control`](crates/ndimon-control/README.md) | HTTP front door: sessions, CSRF, Host allowlist, SSE, static UI, source/health supervisor |
| [`crates/ndimon-state`](crates/ndimon-state/README.md) | The state actor, domain modules, persistence, replay, followers, native backup and plugin host |
| [`crates/ndimon-compat`](crates/ndimon-compat/README.md) | Legacy `/v1` URL adapters that compile each request to one actor command |
| [`crates/ndimon-render`](crates/ndimon-render/README.md) | Pure projection of the document onto the `/etc`, Tessera and ALSA files that other programs read; ownership table |
| [`crates/ndimon-privd`](crates/ndimon-privd/README.md) | Root daemon: typed requests, atomic artifact writer, plugin lifecycle, update agent, network jobs |
| [`crates/ndimon-privd-proto`](crates/ndimon-privd-proto/README.md) | The closed privileged protocol, frame codec and async client |
| [`crates/ndimon-netengine`](crates/ndimon-netengine/README.md) | Network engine: networkd, Netplan, NetworkManager and ifupdown backends, journal, recovery, handoff |
| [`crates/ndimon-tessera`](crates/ndimon-tessera/README.md) | Client for Tessera's `control.sock` contract (no Tessera crate is linked) |
| [`crates/ndimon-release`](crates/ndimon-release/README.md) | Minisign release manifest verification and trusted-key loading |
| [`crates/ndimon-package`](crates/ndimon-package/README.md) | Native Debian maintainer hooks (an ELF program, not a script) and console helpers |
| [`crates/ndimonctl`](crates/ndimonctl/README.md) | `status` and the opt-in `diag collect\|analyze` diagnostics |
| `xtask` | Build, check, export, packaging, install and release tasks |
| `schema/` | JSON Schema written by `export-types` |
| `../ui/src/generated/` | TypeScript written by `export-types` |
| `../packaging/debian/` | `sysusers.d`, `tmpfiles.d`, udev rule and licence texts that the packages carry |
| `../systemd/` | The units: control (and its socket), privd (and its socket), state import, network boot, decoder, finder |

## Build and check

Build and test on a build host or in CI, not on the appliance. The workspace
uses stable Rust, edition 2024, minimum Rust 1.87 (`rust-toolchain.toml` selects
`stable` with `rustfmt` and `clippy`). The plugin builder uses system zlib: install
a C compiler, `pkg-config` and `zlib1g-dev`. Run every cargo command with
`RUSTFLAGS="-D warnings"`, as CI does. From `control/`:

```text
cargo build --workspace
cargo xtask ci
cargo check -p ndimon-types --no-default-features
cargo build --workspace --target aarch64-unknown-linux-gnu
```

`cargo xtask ci` runs, in order, `cargo fmt --all --check`, Clippy with warnings
denied on all targets and features, `cargo test --workspace --all-features`,
`cargo check -p ndimon-types --no-default-features`, `cargo deny check` (skipped
with a message when cargo-deny is not installed) and `check-no-scripts`. The
cross build needs the `aarch64-unknown-linux-gnu` target and
`gcc-aarch64-linux-gnu`; the linker is chosen in `.cargo/config.toml`.

`cargo xtask check-no-scripts` scans the source tree, including untracked and
ignored files, and fails on any `.py`, `.sh`, `.bash`, `.zsh` or `.ksh` file and on
a shell or Python interpreter line, extensionless files included. There is no
allowlist. Version-control, dependency and build output directories are skipped.

The other developer tasks:

| Task | Purpose |
|---|---|
| `cargo xtask build` | CMake build of the C++ decoder and finder into `build/` (extra arguments go to CMake) |
| `cargo xtask export-types` | Regenerate `schema/` and `ui/src/generated/` |
| `cargo xtask build-ui` / `check-ui` | Rebuild the browser bundle with Node 22 (`npm ci`, `npm run build`) / type-check it. Developer and CI only |
| `cargo xtask build-vendor` | Fetch the pinned Bootstrap and Bootstrap Icons tarballs the lockfile names, verify their sha512 `integrity` and unpack the files the page loads. Needs no npm |
| `cargo xtask setup-deps`, `ndi-sdk` | Provision build and runtime dependencies and the licensed NDI SDK |

The C++ build needs CMake, a C++17 compiler, `pkg-config`, `nlohmann-json3-dev`,
`libswresample-dev` and `libavutil-dev`.

## Contract types and exports

`ndimon-types` has these features:

- `serde` (default): JSON serialization and deserialization, impact-token sealing.
- `ts`: ts-rs 11 derives (implies `serde`).
- `schema`: schemars 1 derives (implies `serde`).

`cargo xtask export-types` writes the TypeScript roots and everything they
depend on into `ui/src/generated/`, and one JSON Schema per root into
`control/schema/`. The roots are `DesiredState`, `CommandRequest`,
`CommandReply`, `Origin`, `Event`, `Query`, `ErrorEnvelope`, `Observed`,
`SessionCommand`, `NetworkRequest`, `PluginBackupSection` and `PluginBackupExport`.
Use this task instead of editing generated files; CI regenerates them and fails
on any difference. TypeScript uses JSON numbers for the `u64` revision and
duration fields; browser editors must not do arithmetic on revisions outside
JavaScript's exact integer range (the server's counter is `u64`).

All public enums are non-exhaustive. Command, event, origin, query and error tags
are serde external tags in snake_case; unit variants are plain strings. The
document, its domains and the commands reject unknown keys. `CommandRequest::new`
defaults the optional revision and impact tokens to none and `confirm` to false.
A successful native command answers `{ok: true, revision, report, result}`;
refusals use `ErrorEnvelope` with `ApiError::http_status()`. SSE framing
(`event`, `resync`, keep-alive) is a transport concern of `ndimon-control`.

`validate()` collects every portable field error without I/O. It cannot know
whether an EDID mode exists, a plane supports 180 degrees, an endpoint is online,
a video delay fits current memory, or the host's network configuration can be
adopted; the actor's planner and preflight check those against observed state.

Audio is one domain: the engine choice and the graph are one document and one
transaction. Node factors keep the v1 source gain, sink mix and delay values,
including inactive ones; v2 concrete routes are the runtime authority. A v1 sum
that exceeds the v2 limits is retained and blocks v2 activation until it is
normalised explicitly. An untouched install has an unadopted network (null
backend, empty interface, rows and roles); the engine imports the effective
configuration before the first mutation. Tessera's default name and latency are
read from the daemon, not guessed.

## Packaging and installation

```text
cargo xtask build-deb [--version DEBIAN_VERSION] [--no-build] [-- CMAKE_OPTIONS...]
cargo xtask install [--no-deps] [build-deb options]
cargo xtask build-plugin tessera --bundle /path/to/tessera-bundle_VERSION_ARCH.tar.gz
cargo xtask build-plugin --plugin NAME [--arch amd64|arm64] [--output DIR] [--release-tag TAG]
```

`build-deb` (default version `1.4.0-1`) builds the workspace in release mode, the
decoder and finder with CMake, fetches the vendor assets, and stages four
packages for the machine's architecture into `dist/debian/`, assembled with
`dpkg-deb` directly:

| Package | Contents |
|---|---|
| `ndimon-privd` | Root daemon (`/usr/libexec/ndimon/ndimon-privd`), its socket, service and the network-boot unit |
| `ndimon-control` | `ndimon-control`, `ndimon-state-import`, `ndimonctl`, `ndimon-package`; the control and state-import units; sysusers/tmpfiles; default `/etc` templates |
| `ndimon-ui` | The static UI, Bootstrap and fonts, and Tessera's manifest and page fragment |
| `ndimon-runtime` | `ndimon-r`, `ndimon-finder`, fonts, udev rule, units, `/usr/share/ndimon/version` |

The packages depend on each other at the exact version. Every maintainer script is
a copy of the compiled `ndimon-package` program, which dpkg executes directly. The
builder refuses any package containing a `.sh`/`.py`/`.pyc` file, an interpreter
line (shell, Python or Node), a `node_modules` tree, or Node in a dependency field.
`ndimon-runtime` is not interchangeable between distribution releases:
`dpkg-shlibdeps` records the library sonames of the build machine, so build it on
the distribution the appliance runs.

On upgrade the runtime package's configure hook creates users and directories from
`sysusers.d`/`tmpfiles.d`, seeds missing `/etc` templates, imports the existing
configuration into the state document (`ndimon-state-import`, which keeps
`<file>.pre-control` copies of the originals), runs the network boot job (which
imports earlier journals), retires the files of earlier releases (sudoers rule,
helper scripts, watchdog, updater, the Node API and its unit), and only then
enables and starts the stack. A failed import or network migration stops the
configure hook before those files are removed. See
[the README](../README.md#rollback) for rolling back.

`cargo xtask install` runs `setup-deps` (unless `--no-deps`), builds the packages
and installs them with `apt install --allow-downgrades`. It installs no Node or
npm: the committed UI bundle ships and the vendor files are fetched by
`build-vendor`. `build-plugin` writes `NAME-plugin_VERSION_ARCH.tar.gz` (`ARCH` is
`amd64` or `arm64`, the names a release feed matches: `--arch`, or this build host's
for a plugin without a bundle, or the bundle's for Tessera), its
catalog entry and a shared sorted `SHA256SUMS` under `dist/plugins/`. It checks the
bundle's checksum, manifest and platform against `bundles.lock.json` and
`compatibility.json`, and requires the Tessera runtime to ship its own
`tessera-bundle` installer (Tessera 0.3.0 or later). Archives are deterministic
(sorted, root-owned members with fixed modes, `SOURCE_DATE_EPOCH` defaulting to
zero, zlib level 9) and byte-identical to the archives earlier releases built.

## Signed releases

```text
cargo xtask release --version VERSION --out DIR FILE...
cargo xtask verify-release --dir DIR --version VERSION --public-key FILE
cargo xtask release-smoke --dir DIR --version VERSION
cargo xtask release-keygen --out NEW_PRIVATE_DIR
```

`release` collects explicit prebuilt artifacts and signs `SHA256SUMS` with the key
named by `NDIMON_RELEASE_KEY_FILE`, unlocked by `NDIMON_RELEASE_KEY_PASSWORD`
(both required; nothing is logged). `verify-release` checks the signature's
product and version binding and every listed checksum. `release-smoke` signs and
verifies the real package set with a throwaway in-memory key and checks that
tampering is refused. `release-keygen` is an owner operation that writes an
encrypted 0600 secret key and the public key.

The compiled trust list is empty. A device that has no trusted key accepts no
signed update and is updated by installing a verified package set by hand. Public
keys can be added in the root-owned `/etc/ndimon/trusted-release-keys`. See
[Signed releases and updates](../docs/releases.md) for key custody, rotation, the
update agent's transport limits and the per-distribution runtime build.
