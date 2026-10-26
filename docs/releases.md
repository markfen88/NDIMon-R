# Signed releases and updates

A release is a set of prebuilt Debian packages with `SHA256SUMS` and
`SHA256SUMS.minisig`. The checksum manifest is signed with minisign Ed25519
(prehashed `ED`). Device verification accepts only `ED`, the same rule the
Tessera runtime uses; legacy `Ed` signatures are refused. Both the manifest and
its trusted comment are authenticated before package checksums are used. The
trusted comment is exactly:

```text
product:ndimon-r<TAB>version:1.4.0-1<TAB>file:SHA256SUMS
```

`<TAB>` denotes a literal tab. Versions are Debian versions without the tag's
leading `v`. Checksums use GNU coreutils records with LF endings and flat,
unique asset basenames. Escaped names, paths, blank lines and self-references
are refused. All selected package contents must match before installation.

Each architecture contributes exactly four packages at the same Debian version:

- `ndimon-control`: control plane, `ndimon-state-import`, `ndimonctl`, the native
  package-hook and console utility, defaults and the sysusers/tmpfiles
  declarations.
- `ndimon-privd`: root daemon, activation socket and the network boot unit.
- `ndimon-runtime`: C++ decoder and finder, fonts, units and version metadata.
- `ndimon-ui`: static UI, bundled TypeScript and vendor styles/fonts.

No package depends on, recommends or ships Node.js: the builder refuses Node
shebangs, `node_modules` trees and Node package names in any dependency field.
`ndimon-control` declares `Conflicts`/`Replaces: ndimon-legacy-api`, so an upgrade
from an intermediate build that still had that package removes it.

## Trust: who signs, and which keys a device accepts

**No project signing key is compiled into the packages.** The built-in list of
trusted keys is empty. Without a provisioned public key every verification
fails with `untrusted_key`, and setting the update switch alone does not create
trust. A device installs only releases whose `SHA256SUMS` carries a signature
from a key it trusts.

A deployment provisions its first trusted key as a root-owned file on each
device:

```sh
sudo install -d -o root -g root -m 0755 /etc/ndimon
sudo install -o root -g root -m 0644 ./trusted-release-keys /etc/ndimon/trusted-release-keys
```

`trusted-release-keys` holds one bare minisign public key per line (the second
line of a `.pub` file), with no comments. All ancestors must be root-owned and
not group- or world-writable and no component may be a symlink; the file itself
must be a root-owned regular file with mode exactly 0644. The file **adds** keys
to the compiled list; it cannot revoke a compiled key. Root provisioning
establishes first trust through your own administrative channel: downloading a
key from the release being verified establishes nothing.

The signing key belongs to whoever publishes the release. Choose who holds the
offline encrypted backup, where it is stored, who knows its password, and who may
edit the CI secrets. Keep the secret key and its password out of source control,
chat, job logs and release artifacts. The commands below are a **procedure for a
trusted maintainer machine**, never for an unattended job or an appliance. From
a private checkout's `control/` directory, choose a new private directory outside
the checkout and enter a password without echoing it:

```sh
read -rsp 'New release-key password: ' NDIMON_RELEASE_KEY_PASSWORD
export NDIMON_RELEASE_KEY_PASSWORD
cargo xtask release-keygen --out /private/ndimon-release-key
unset NDIMON_RELEASE_KEY_PASSWORD
```

The output directory must not already exist. The task writes an encrypted
`ndimon-release.key` (0600) and `ndimon-release.pub` (0644) in a 0700 directory
and prints only the bare public key. It refuses an empty password or an existing
directory or file; a failed run can leave a private partial directory to remove
before retrying. Set the CI secrets `NDIMON_RELEASE_KEY` (the complete encrypted
key file) and `NDIMON_RELEASE_KEY_PASSWORD`. The signing job creates a 0600
temporary key file with shell tracing disabled, removes it with an exit trap and
uploads only the signed output directory; it never generates a key. Restrict
release tags and workflow edits to trusted maintainers, because a signing job can
use the CI secret.

## Build, sign and verify

Build releases on a build host or in CI; the source installer also supports
building directly on the appliance. A release runner needs, for each
architecture: stable Rust (an exact installed version, for example `1.99.1`;
moving aliases such as `stable` are refused), dpkg-dev, Node 22 and npm (UI build
tool only), CMake and the C++ dependencies, and the licensed NDI SDK, which
this workflow does not redistribute. Both architectures must build before
anything is signed.

Pushing a tag `vVERSION` runs the release workflow. On each architecture,
`cargo xtask build-deb --version VERSION` builds the Rust workspace, the C++
decoder and finder with CMake, and the browser TypeScript with its vendored
Bootstrap assets. The same task is used by the on-appliance installer. All cargo
commands in CI use `RUSTFLAGS="-D warnings"` and two build jobs.

The Rust package builder stages explicit assets and invokes `dpkg-deb` directly,
so every maintainer hook is a compiled Rust ELF executable. It deliberately does
not use debhelper's generated shell activation scripts. It rejects Python and
shell extensions and interpreter shebangs in every staged payload, including
extensionless hooks. `dpkg-shlibdeps` discovers the C++ dependencies for the build
distribution. The licensed NDI SDK is provisioned separately by
`cargo xtask setup-deps` rather than redistributed without its license.

### The runtime package is built per distribution release

`ndimon-control`, `ndimon-privd` and `ndimon-ui` are plain Rust and static
assets; the same bytes install on every supported release. **`ndimon-runtime` is
not interchangeable**: it holds the C++ decoder and finder, and
`dpkg-shlibdeps` writes the sonames of the libraries on the build machine into
its `Depends`. A package built on Debian 13 names FFmpeg 7.1
(`libavutil59`, `libswresample5`) and cannot be installed on Ubuntu 26.04, which
only has `libavutil60`, `libswresample6` and `libavcodec62`; the reverse is
true as well. Build the runtime package on each distribution release the
appliance runs (or in a clean image of it) and publish one per release, named
for it, next to the three shared packages. A practical recipe for an extra
distribution is to build the C++ part in an image of that distribution, reuse the
Rust binaries, then run `cargo xtask build-deb --no-build`.

Dependencies that `dpkg-shlibdeps` cannot see are added by the builder. The
NDI library is installed separately and links `libavahi-client.so.3` and
`libavahi-common.so.3`, which the decoder and finder only reach through it, so
`ndimon-runtime` depends on `libavahi-client3` and `libavahi-common3`. Packages
that ship units depend on `systemd (>= 244)`: the network boot unit relies on
`Restart=on-failure` for a oneshot service.

### Package hooks

Native hooks stop the writers and remove shadowing manual `/etc/systemd/system`
units (preserving `*.pre-p8` copies). Once all four packages are available, the
runtime's configure hook creates the declared accounts and directories, imports
the 1.3.x configuration, runs the network engine's journal recovery and import,
and only then removes the sudoers rule, the old helpers and the Python engine
selector. Original configuration stays in `.pre-control` files; private network
sources are preserved in `.pre-p8` snapshots, made only of data that was there
before (the empty directories a fresh install creates are not copied). A
migration failure prevents helper removal and activation. The control plane and
the privd socket are always enabled. Removal and purge also delete the dangling
enablement links of the package's own units. Shared-tree directory ownership comes
only from tmpfiles, including after service restarts.

### Check a build

Run `cargo xtask release-smoke --dir ../dist/debian --version VERSION` to sign
and verify real package bytes with a throwaway in-memory key and check tamper
rejection. This creates no project trust. Lint every package with
`lintian --fail-on error ../dist/debian/*.deb`. Then verify a fresh installation
and an upgrade from 1.3.x before publishing. Lack of DRM/NDI hardware must be
recorded separately from package and web-login checks.

Compiler version details are included as architecture-specific artifacts.
The final `signed-release` workflow artifact combines both architectures under
one signed checksum manifest. The workflow performs no repository publication.

To sign explicit prebuilt artifacts by hand on the signing machine:

```sh
export NDIMON_RELEASE_KEY_FILE=/private/ndimon-release-key/ndimon-release.key
read -rsp 'Release-key password: ' NDIMON_RELEASE_KEY_PASSWORD
export NDIMON_RELEASE_KEY_PASSWORD
cargo xtask release --version 1.4.0-1 --out ../dist/release ../dist/artifacts/*.deb
unset NDIMON_RELEASE_KEY_FILE NDIMON_RELEASE_KEY_PASSWORD
cargo xtask verify-release --dir ../dist/release --version 1.4.0-1 --public-key /private/ndimon-release-key/ndimon-release.pub
```

The output directory must be empty initially. Artifact paths are explicit; the
task does not scan the checkout, include tests or plans, or publish source or
history. It refuses the configured signing-key file as an artifact.
`verify-release` verifies every listed artifact and accepts either a standard
two-line public key file or a single bare public-key line. Verification needs
only public data.

### Verify a download by hand

Obtain the public key through a trusted channel and, inside the downloaded release
directory, run the independent minisign CLI:

```sh
minisign -Vm SHA256SUMS -p /trusted/path/ndimon-release.pub -x SHA256SUMS.minisig
sha256sum --check --strict SHA256SUMS
```

Run the checksum command only after minisign succeeds. Confirm minisign's
authenticated trusted comment names **ndimon-r**, the exact intended version
and **SHA256SUMS**. The CLI's crypto check alone does not enforce the product
or version; the Rust verifier enforces all three automatically.

## Publication

Publishing a release to GitHub is a manual, attended step. Verify that the
destination repository is public and review the exact artifact list first.
Download the `signed-release` artifact, verify it independently as above, and
attach the packages, compiler records, `SHA256SUMS` and `SHA256SUMS.minisig` to the
matching `vVERSION` release with the exact filenames and bytes. If the runtime and
build sources are published too, prepare a sanitized snapshot from the existing
public branch and leave out private history, agent files, tests, testing results
and methodology, and internal plans. The CI workflow has no GitHub mirror or
publication step, and configuring signing secrets or tagging a release does not
publish anything.

## Device update operation

The optional `/etc/ndimon/release-source` is a root-owned 0644 regular file
containing only `owner/repository`; the default is `markfen88/NDIMon-R`. Its
ancestors have the same trust requirements as the key file. It cannot supply a
URL, credentials, an alternate host or an arbitrary path. Releases are resolved
with the GitHub releases API for the exact `vVERSION` tag; drafts are refused.

### Finding the newest release

With a trusted key configured, the update agent can also say which release the
device could install, so that the web **Update now** button has a version to
install. The `check_update` privd request (sent by the control plane when a page
asks for the version) uses the same source, key and host rules as an install and
makes only HTTPS requests to `api.github.com` (without redirects) and to the
repository's release assets:

1. `GET /repos/OWNER/REPO/releases/latest`: the release the project marks as
   latest (a draft or prerelease never is). Its tag must be `v` plus a valid
   Debian version, with at most 128 assets.
2. The version is compared with `/etc/ndimon-firmware-version` with
   `dpkg --compare-versions VERSION gt INSTALLED`. A release that is not newer is
   answered as "up to date" at once, with no further request.
3. For a newer release the agent downloads `SHA256SUMS` and `SHA256SUMS.minisig`
   (the same asset URL rules and limits as an install) and verifies the signature
   against the trusted keys, the trusted comment (product and this version) and
   the checksum list. The list must name the four packages for this architecture.
   Only then is the version reported as installable. Packages are not downloaded.

An answer is never reported on unverified data: a wrong signature, product or
version, a missing signature asset, a draft, an invalid tag or an incomplete
package set fails the lookup instead. The lookup is bounded: connection 4 s, read
5 s, 8 s per request, and no request starts later than 20 s after the first. The
control plane keeps the answer for one hour and a failed answer for ten minutes,
shares it between callers (one lookup runs at a time) and asks only when a page
queries the version, so an offline device is not penalised and GitHub's 60
anonymous API requests an hour per address are not exhausted. The version query
(`GET /v1/System/version`, `/api/v1/system/version`) then reports, in addition to
the usual fields:

| Field | Meaning |
|---|---|
| `latest_version` | Present only for a verified release that is newer than the device |
| `updates_available` | `1` with `latest_version`, `0` when nothing newer is signed; absent when the lookup failed |
| `update_message` | `release VERSION is available`, `no newer signed release`, or `could not check for a signed release: REASON` (offline, rate limited, `bad_signature`, …) |

Without a trusted key the lookup is not made: `update_supported` is false and the
message stays "updates need a signed release; no release key is configured". The
install job itself re-verifies everything and refuses a downgrade unless forced.

`InstallUpdate { version, force: false }` starts an independent transient systemd
job, returned in the action output as `job`. On the wire the request is
`{"install_update":{"version":"1.4.0-1","force":false}}`; an omitted `force`
defaults to false. The job status reports the durable `result` and
`data.progress` with the version, the stage and the current file name. The job is
named `ndimon-plugin-update-N.service`, reusing the closed job type. Jobs run for
at most 30 minutes, survive the disconnection of the caller and the restart of the
control plane, and use an exclusive update lock. Poll until a durable result
exists; a missing result or a stopped worker is a failure, even if systemd says
the unit is inactive.

The worker downloads the signed manifest and only the `.deb` assets for the
local `arm64`/`amd64` architecture (plus `all`). It checks every selected
checksum, then confirms Debian package identity, exact version and architecture
with `dpkg-deb`. A release must contain exactly one each of `ndimon-control`,
`ndimon-privd`, `ndimon-runtime` and `ndimon-ui`; unrelated packages (including a
stale `ndimon-legacy-api`) and duplicates are refused. Both the firmware and the
installed package versions are checked with dpkg's Debian comparison. A downgrade
requires the explicit boolean `force`; comparison errors fail closed. Force never
bypasses signatures, hashes or identities.

Downloads have 15-second connection and 30-second read limits, a 120-second
overall request deadline and at most four redirects. DNS resolution can outlast
the request deadline in the HTTP library; the systemd runtime bound still applies.
Metadata and manifests are limited to 1 MiB, signatures to 4 KiB, each package to
256 MiB, and the package contents to 1 GiB in total with at most 32 selected
assets. Both advertised and streamed response sizes are checked.

The initial metadata request goes to `api.github.com` without redirects. Initial
asset URLs must belong to the configured repository and tag on `github.com`.
Asset redirects allow only HTTPS `github.com`, `objects.githubusercontent.com`
and `release-assets.githubusercontent.com` (where GitHub serves release assets
today), exact host names, with no credentials, non-443 ports or fragments. The
host list bounds where privd connects; integrity comes from the signed
`SHA256SUMS`, so a package from any host must still match it. No live release
lookup or download from GitHub has been verified yet (both are tested against a
fake release source), so try the first update on a spare device.

Packages are streamed into a root-private temporary directory under
`/var/lib/ndimon-updates`, all verified before a fixed
`dpkg --force-confdef --force-confold -i -- FILE...` invocation, and removed when
the worker completes. Nobody can answer a conffile question during an update, so
an operator's edit of a conffile always wins over a changed default; the list of
certified network backends
(`/etc/ndimon-network-certified-backends.json`) is one. A killed worker may leave
staging files; a root administrator may remove abandoned `release-*` directories
when no update is running. A partial dpkg installation is reported as a failure;
it is not a transactional rollback. Check the journal and job results and repair
the package state through the normal administrative procedure before retrying.
The firmware marker advances only after dpkg succeeds.

Signed updates are the only update path; there is no switch to turn them on.
`NDIMON_PRIVD=0` in the control unit's environment makes update requests refuse.
`POST /v1/System/update` (and the `install_update` command)
requires an explicit `version` and an optional boolean `force`. The old
checkout updater and the `StartUpdate` operation are gone; privd keeps the wire
tag only to refuse it, and no `ndimon-update` unit is installed. Selecting a
release in the visible UI is not offered: the web **Update now** button installs
the version the device reports as `latest_version` (the newest verified release,
see [Finding the newest release](#finding-the-newest-release)). To install a
specific version use the API or an administrative apt installation. Trust must be
provisioned first; installation never generates a key, and without one the page
says that updates are manual.

## Upgrade and rollback

On-device builds use `cargo xtask install` (or `--no-deps`); the old install,
update and dependency-setup shell scripts no longer exist. apt installs the full
package set from `dist/debian`. An upgrade from a 1.3.x appliance keeps its
password and every representable saved setting and preserves the unknown original
bytes; see [Upgrading from 1.3.x](../README.md#upgrading-from-13x). The Python
network engine and its fallback switch are gone; privileged actions always use
Rust.

Follow the [exact rollback procedure](../README.md#rollback): stop the stack,
reinstall the full previous release, and restore `state.prev.json` only when that
one-revision recovery point is intended. A rollback to 1.3.x also restores the
`.pre-control` settings and the `.pre-p8` private network snapshots before
running the saved old release installer. Keep the old artifacts or checkout,
backups and console access available. A dpkg install is not an atomic release
transaction and a partial failure must be repaired explicitly.

## Key rotation

1. Generate a new key with the same custody procedure.
2. Add its **public** key alongside the old key in the compiled trust list (or
   provision it as an additive root-owned override file on a development
   device).
3. Build and ship that version **signed by the old key**. Verify that target
   devices have installed it and trust the new key before switching the signer.
4. Move CI to the new encrypted signing key and password, verify a new-key release
   on already upgraded devices, then remove the old public key in a later
   release. Remove any retired override entry separately; overrides cannot revoke
   still-compiled keys. Keep recovery material for older devices according to
   your custody policy.

Never replace the only trusted key and sign that transition solely with the new
key. NDIMon-R and Tessera share this signing contract and the task names
(`release`, `verify-release`), without sharing implementation code.
