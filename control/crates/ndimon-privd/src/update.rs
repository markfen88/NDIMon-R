//! Signed updates run only in a root system-manager job, never in the API.
use crate::{Host, process::args, store::io_error};
use ndimon_privd_proto::{ActionOutput, Error, ReleaseVersion};
use ndimon_release::Manifest;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, Permissions},
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    time::{Duration, Instant},
};
use url::Url;

const DEFAULT_SOURCE: &str = "markfen88/NDIMon-R";
const MAX_API_BYTES: usize = 1024 * 1024;
/// A lookup is a page load's side trip: its requests stop being started after
/// this long, and each one is cut off sooner than an install's would be.
const LOOKUP_BUDGET: Duration = Duration::from_secs(20);
const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
const REQUIRED_PACKAGES: [&str; 4] = [
    "ndimon-control",
    "ndimon-privd",
    "ndimon-runtime",
    "ndimon-ui",
];

struct HttpResponse {
    status: u16,
    location: Option<String>,
    length: Option<u64>,
    body: Box<dyn Read>,
}
/// The policy lives above the transport, so tests cannot bypass bounds or
/// redirect checks by supplying a different HTTP implementation.
trait HttpClient {
    fn get(&self, url: &Url) -> Result<HttpResponse, Error>;
}
struct Https {
    agent: ureq::Agent,
}
impl Https {
    fn new() -> Self {
        Self::bounded(15, 30, 15, 120)
    }
    /// Timeouts in seconds: connect, read, write and the whole request.
    fn bounded(connect: u64, read: u64, write: u64, request: u64) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .redirects(0)
                .timeout_connect(Duration::from_secs(connect))
                .timeout_read(Duration::from_secs(read))
                .timeout_write(Duration::from_secs(write))
                .timeout(Duration::from_secs(request))
                .build(),
        }
    }
}
impl HttpClient for Https {
    fn get(&self, url: &Url) -> Result<HttpResponse, Error> {
        let response = match self
            .agent
            .get(url.as_str())
            .set("User-Agent", "ndimon-r-signed-update")
            .set(
                "Accept",
                if url.host_str() == Some("api.github.com") {
                    "application/vnd.github+json"
                } else {
                    "application/octet-stream"
                },
            )
            .set("Accept-Encoding", "identity")
            .call()
        {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(_) => return Err(Error::failed("download", "HTTPS transport failed")),
        };
        let length = response
            .header("Content-Length")
            .map(str::parse::<u64>)
            .transpose()
            .map_err(|_| Error::failed("download", "invalid content length"))?;
        Ok(HttpResponse {
            status: response.status(),
            location: response.header("Location").map(String::from),
            length,
            body: response.into_reader(),
        })
    }
}

/// Refuses every request once a whole lookup has used up its time, so that the
/// requests it makes one after another stay within one bound.
struct Deadline<'a> {
    http: &'a dyn HttpClient,
    until: Instant,
}
impl HttpClient for Deadline<'_> {
    fn get(&self, url: &Url) -> Result<HttpResponse, Error> {
        if Instant::now() >= self.until {
            return Err(Error::failed("release", "the release lookup took too long"));
        }
        self.http.get(url)
    }
}

fn safe_https(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.fragment().is_none()
}
/// Hosts GitHub serves release assets from (redirect targets). Integrity comes
/// from the signed SHA256SUMS, not from the host; the list only bounds where
/// privd connects. GitHub moved asset downloads to release-assets in 2025.
fn allowed_asset_host(url: &Url) -> bool {
    safe_https(url)
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "objects.githubusercontent.com"
                    | "release-assets.githubusercontent.com"
            )
        )
}
fn response(
    http: &dyn HttpClient,
    initial: Url,
    api: bool,
    limit: u64,
) -> Result<HttpResponse, Error> {
    let mut url = initial;
    for hop in 0..=4 {
        let allowed = if api {
            hop == 0 && safe_https(&url) && url.host_str() == Some("api.github.com")
        } else {
            allowed_asset_host(&url)
        };
        if !allowed {
            return Err(Error::failed(
                "download",
                "host is outside the HTTPS release allowlist",
            ));
        }
        let reply = http.get(&url)?;
        if matches!(reply.status, 301 | 302 | 303 | 307 | 308) {
            if api || hop == 4 {
                return Err(Error::failed(
                    "download",
                    "unexpected API redirect or too many asset redirects",
                ));
            }
            let target = reply
                .location
                .as_deref()
                .ok_or_else(|| Error::failed("download", "redirect lacks location"))?;
            url = url
                .join(target)
                .map_err(|_| Error::failed("download", "invalid redirect"))?;
            continue;
        }
        if reply.status != 200 {
            return Err(Error::failed(
                "download",
                "release server did not return HTTP 200",
            ));
        }
        if reply.length.is_some_and(|n| n > limit) {
            return Err(Error::failed(
                "download",
                "content length exceeds download limit",
            ));
        }
        return Ok(reply);
    }
    Err(Error::failed("download", "redirect limit exceeded"))
}
fn copy_bounded(mut reply: HttpResponse, output: &mut dyn Write, limit: u64) -> Result<u64, Error> {
    let mut total = 0;
    let mut buffer = [0; 64 * 1024];
    loop {
        // Read at most one byte beyond the bound, even with chunked responses.
        let room = (limit - total + 1).min(buffer.len() as u64) as usize;
        let count = reply
            .body
            .read(&mut buffer[..room])
            .map_err(|_| Error::failed("download", "response body read failed"))?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > limit {
            return Err(Error::failed(
                "download",
                "response body exceeds download limit",
            ));
        }
        output.write_all(&buffer[..count]).map_err(io_error)?;
    }
    if reply.length.is_some_and(|n| n != total) {
        return Err(Error::failed(
            "download",
            "response length does not match content length",
        ));
    }
    Ok(total)
}
fn download_bytes(
    http: &dyn HttpClient,
    url: Url,
    api: bool,
    max: usize,
) -> Result<Vec<u8>, Error> {
    let reply = response(http, url, api, max as u64)?;
    let mut bytes = Vec::new();
    copy_bounded(reply, &mut bytes, max as u64)?;
    Ok(bytes)
}

fn valid_source(source: &str) -> bool {
    let parts: Vec<_> = source.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|s| {
            !s.is_empty()
                && s.len() <= 100
                && !s.starts_with('.')
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}
/// `releases/tags/vVERSION`, or `releases/latest` (the release the project marks
/// as its latest) when no version is given.
fn api_url(source: &str, version: Option<&ReleaseVersion>) -> Result<Url, Error> {
    let mut url = Url::parse("https://api.github.com/")
        .map_err(|_| Error::failed("release", "invalid API base"))?;
    {
        let mut parts = url
            .path_segments_mut()
            .map_err(|_| Error::failed("release", "invalid API URL"))?;
        parts.clear().push("repos");
        for part in source.split('/') {
            parts.push(part);
        }
        parts.push("releases");
        match version {
            Some(version) => parts.push("tags").push(&format!("v{version}")),
            None => parts.push("latest"),
        };
    }
    Ok(url)
}
#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    assets: Vec<Asset>,
}
#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}

fn asset_url(asset: &Asset, source: &str, version: &ReleaseVersion) -> Result<Url, Error> {
    let url = Url::parse(&asset.browser_download_url)
        .map_err(|_| Error::failed("release", "invalid asset URL"))?;
    let mut expected = Url::parse("https://github.com/")
        .map_err(|_| Error::failed("release", "invalid asset base"))?;
    {
        let mut parts = expected
            .path_segments_mut()
            .map_err(|_| Error::failed("release", "invalid asset URL"))?;
        parts.clear();
        for part in source.split('/') {
            parts.push(part);
        }
        parts
            .push("releases")
            .push("download")
            .push(&format!("v{version}"))
            .push(&asset.name);
    }
    if !allowed_asset_host(&url) || url != expected {
        return Err(Error::failed(
            "release",
            "asset URL does not belong to the requested repository and tag",
        ));
    }
    Ok(url)
}

fn checked_version(text: &str) -> Result<ReleaseVersion, Error> {
    text.trim()
        .to_owned()
        .try_into()
        .map_err(|_| Error::failed("version", "invalid installed or package version"))
}
fn refuse_downgrade(
    host: &Host,
    requested: &ReleaseVersion,
    current: &ReleaseVersion,
    force: bool,
) -> Result<(), Error> {
    if force {
        return Ok(());
    }
    match host.runner.run(
        "/usr/bin/dpkg",
        &args(&[
            "--compare-versions",
            requested.as_str(),
            "lt",
            current.as_str(),
        ]),
        None,
    ) {
        Ok(_) => Err(Error::invalid("version", "downgrade requires force")),
        Err(Error::Failed {
            exit_code: Some(1), ..
        }) => Ok(()),
        Err(error) => Err(error),
    }
}
fn installed_package_version(host: &Host, package: &str) -> Result<Option<ReleaseVersion>, Error> {
    match host.runner.run(
        "/usr/bin/dpkg-query",
        &args(&[
            "--show",
            "--showformat=${db:Status-Status}\t${Version}",
            package,
        ]),
        None,
    ) {
        Ok(out) => {
            let (status, version) =
                out.stdout.trim().split_once('\t').ok_or_else(|| {
                    Error::failed("version", "cannot parse installed package state")
                })?;
            match status {
                "installed" | "unpacked" | "half-configured" | "half-installed" => {
                    Ok(Some(checked_version(version)?))
                }
                "not-installed" | "config-files" => Ok(None),
                _ => Err(Error::failed(
                    "version",
                    "unrecognized installed package state",
                )),
            }
        }
        Err(Error::Failed {
            exit_code: Some(1), ..
        }) => Ok(None),
        Err(error) => Err(error),
    }
}

fn progress(
    host: &Host,
    id: u64,
    version: &ReleaseVersion,
    stage: &str,
    file: Option<&str>,
) -> Result<(), Error> {
    let data = serde_json::to_vec(
        &serde_json::json!({"version":version.as_str(),"stage":stage,"file":file}),
    )
    .map_err(|e| Error::failed("progress", e))?;
    host.root.atomic(
        &format!("/run/ndimon/privd-jobs/{id}.progress.json"),
        &data,
        nix::unistd::getegid().as_raw(),
        0o600,
    )?;
    tracing::info!(stage, "signed update progress");
    Ok(())
}
fn verification_error(error: ndimon_release::Error) -> Error {
    Error::failed(error.code(), error)
}

/// The keys releases must be signed with: the compiled list and the root-owned
/// override file. Without one nothing can be verified, so nothing is installed
/// or offered.
fn trusted_keys(host: &Host) -> Result<Vec<String>, Error> {
    let keys =
        ndimon_release::trust::load_keys(&host.root.path(ndimon_release::trust::OVERRIDE_FILE))
            .map_err(verification_error)?;
    if keys.is_empty() {
        return Err(verification_error(ndimon_release::Error::UntrustedKey));
    }
    Ok(keys)
}
/// The `owner/repository` the releases are published in.
fn release_source(host: &Host) -> Result<String, Error> {
    let source =
        ndimon_release::trust::read_config(&host.root.path("/etc/ndimon/release-source"), 256)
            .map_err(verification_error)?
            .unwrap_or_else(|| DEFAULT_SOURCE.into());
    let source = source.trim();
    if !valid_source(source) {
        return Err(Error::invalid(
            "release_source",
            "expected owner/repository slug",
        ));
    }
    Ok(source.to_owned())
}
fn installed_version(host: &Host) -> Result<ReleaseVersion, Error> {
    let current = host
        .root
        .read("/etc/ndimon-firmware-version", 256)?
        .ok_or_else(|| Error::failed("version", "installed firmware version is unavailable"))?;
    let current = std::str::from_utf8(&current)
        .map_err(|_| Error::failed("version", "installed firmware version is not UTF-8"))?;
    checked_version(current)
}
fn architecture(host: &Host) -> Result<String, Error> {
    let arch = host
        .runner
        .run("/usr/bin/dpkg", &args(&["--print-architecture"]), None)?
        .stdout
        .trim()
        .to_owned();
    if !matches!(arch.as_str(), "amd64" | "arm64") {
        return Err(Error::failed(
            "architecture",
            "signed updates support amd64 and arm64",
        ));
    }
    Ok(arch)
}
/// A release's assets by name; a repeated name is refused.
fn asset_map(release: &Release) -> Result<BTreeMap<&str, &Asset>, Error> {
    let mut assets = BTreeMap::new();
    for asset in &release.assets {
        if assets.insert(asset.name.as_str(), asset).is_some() {
            return Err(Error::failed("release", "duplicate release asset"));
        }
    }
    Ok(assets)
}
/// Download the checksum list of a release and authenticate it: a signature
/// from a trusted key whose comment names this product and version.
fn verified_manifest(
    http: &dyn HttpClient,
    source: &str,
    version: &ReleaseVersion,
    keys: &[String],
    assets: &BTreeMap<&str, &Asset>,
) -> Result<Manifest, Error> {
    let get = |name: &str, max: usize| -> Result<Vec<u8>, Error> {
        let asset = assets.get(name).ok_or_else(|| {
            verification_error(ndimon_release::Error::MissingFile { name: name.into() })
        })?;
        if asset.size > max as u64 {
            return Err(Error::failed("download", "release asset exceeds limit"));
        }
        download_bytes(http, asset_url(asset, source, version)?, false, max)
    };
    let sums = get("SHA256SUMS", ndimon_release::MAX_MANIFEST_BYTES)?;
    let signature = String::from_utf8(get(
        "SHA256SUMS.minisig",
        ndimon_release::MAX_SIGNATURE_BYTES,
    )?)
    .map_err(|_| Error::failed("malformed", "signature is not UTF-8"))?;
    ndimon_release::verify_manifest(&sums, &signature, keys, version.as_str())
        .map_err(verification_error)
}
/// The packages of a manifest for this architecture (and the architecture-
/// independent ones).
fn package_names<'a>(manifest: &'a Manifest, arch: &str) -> Result<Vec<&'a str>, Error> {
    let suffix = format!("_{arch}.deb");
    let names: Vec<_> = manifest
        .names()
        .filter(|name| name.ends_with(&suffix) || name.ends_with("_all.deb"))
        .collect();
    if names.is_empty() || names.len() > 32 {
        return Err(Error::failed(
            "release",
            "release has no packages for this architecture or too many packages",
        ));
    }
    Ok(names)
}

/// The newest release this device could install, or `None` when it is not newer
/// than what runs. Only what a trusted key signed and what holds the four
/// packages for this architecture counts: unverified data never reaches the
/// answer, and the install job verifies everything again.
fn lookup(
    host: &Host,
    http: &dyn HttpClient,
    keys: &[String],
    source: &str,
) -> Result<Option<ReleaseVersion>, Error> {
    let arch = architecture(host)?;
    let bytes = download_bytes(http, api_url(source, None)?, true, MAX_API_BYTES)?;
    let release: Release = serde_json::from_slice(&bytes)
        .map_err(|_| Error::failed("release", "malformed release API response"))?;
    let version = release
        .tag_name
        .strip_prefix('v')
        .and_then(|tag| ReleaseVersion::try_from(tag.to_owned()).ok())
        .ok_or_else(|| Error::failed("release", "the latest release has no valid version"))?;
    if release.draft || release.assets.len() > 128 {
        return Err(Error::failed("release", "draft or too many release assets"));
    }
    let current = installed_version(host)?;
    let newer = match host.runner.run(
        "/usr/bin/dpkg",
        &args(&[
            "--compare-versions",
            version.as_str(),
            "gt",
            current.as_str(),
        ]),
        None,
    ) {
        Ok(_) => true,
        Err(Error::Failed {
            exit_code: Some(1), ..
        }) => false,
        Err(error) => return Err(error),
    };
    if !newer {
        return Ok(None);
    }
    let assets = asset_map(&release)?;
    let manifest = verified_manifest(http, source, &version, keys, &assets)?;
    // Exactly what an install accepts: one of each required package, and no other.
    let names = package_names(&manifest, &arch)?;
    let one_of_each = |package: &&str| {
        names
            .iter()
            .filter(|name| name.starts_with(&format!("{package}_")))
            .count()
            == 1
    };
    if names.len() != REQUIRED_PACKAGES.len() || !REQUIRED_PACKAGES.iter().all(one_of_each) {
        return Err(Error::failed(
            "release",
            "release must include control, privd, runtime and UI packages",
        ));
    }
    Ok(Some(version))
}
/// `{"latest_version": VERSION | null}`: see `lookup`. Bounded: no request is
/// started 20 s after the first, and each is cut off after 8 s.
pub fn check(host: &Host) -> Result<ActionOutput, Error> {
    let keys = trusted_keys(host)?;
    let source = release_source(host)?;
    let https = Https::bounded(4, 5, 5, 8);
    let http = Deadline {
        http: &https,
        until: Instant::now() + LOOKUP_BUDGET,
    };
    let latest = lookup(host, &http, &keys, &source)?;
    let data = serde_json::json!({"latest_version": latest.map(|version| version.to_string())});
    Ok(ActionOutput {
        data: Some(data),
        ..ActionOutput::default()
    })
}

pub fn install(
    host: &Host,
    version: &ReleaseVersion,
    force: bool,
    id: u64,
) -> Result<ActionOutput, Error> {
    let jobs = host.root.directory("/run/ndimon/privd-jobs", true, true)?;
    // try_lock, unlike the shared plugin lock, refuses competing update jobs.
    let _lock = jobs.try_lock(".signed-update-lock")?;
    let keys = trusted_keys(host)?;
    let source = release_source(host)?;
    let http = Https::new();
    let update = Update {
        host,
        http: &http,
        keys: &keys,
        source: &source,
        version,
        force,
        id,
    };
    let result = update.run();
    if result.is_err() {
        let _ = progress(host, id, version, "failed", None);
    }
    result
}

struct Update<'a> {
    host: &'a Host,
    http: &'a dyn HttpClient,
    keys: &'a [String],
    source: &'a str,
    version: &'a ReleaseVersion,
    force: bool,
    id: u64,
}
impl Update<'_> {
    fn run(&self) -> Result<ActionOutput, Error> {
        let host = self.host;
        progress(host, self.id, self.version, "checking_version", None)?;
        let current = installed_version(host)?;
        refuse_downgrade(host, self.version, &current, self.force)?;
        let arch = architecture(host)?;
        progress(host, self.id, self.version, "fetching_manifest", None)?;
        let bytes = download_bytes(
            self.http,
            api_url(self.source, Some(self.version))?,
            true,
            MAX_API_BYTES,
        )?;
        let release: Release = serde_json::from_slice(&bytes)
            .map_err(|_| Error::failed("release", "malformed release API response"))?;
        if release.draft
            || release.tag_name != format!("v{}", self.version)
            || release.assets.len() > 128
        {
            return Err(Error::failed(
                "release",
                "wrong tag, draft or too many release assets",
            ));
        }
        let assets = asset_map(&release)?;
        let manifest = verified_manifest(self.http, self.source, self.version, self.keys, &assets)?;
        let names = package_names(&manifest, &arch)?;
        let _stage_root = host.root.directory("/var/lib/ndimon-updates", true, true)?;
        let stage = tempfile::Builder::new()
            .prefix("release-")
            .permissions(Permissions::from_mode(0o700))
            .tempdir_in(host.root.path("/var/lib/ndimon-updates"))
            .map_err(io_error)?;
        let mut total = 0;
        let mut paths = Vec::new();
        let mut packages = BTreeSet::new();
        for name in names {
            progress(
                host,
                self.id,
                self.version,
                "downloading_package",
                Some(name),
            )?;
            let asset = assets.get(name).ok_or_else(|| {
                verification_error(ndimon_release::Error::MissingFile { name: name.into() })
            })?;
            if asset.size > MAX_PACKAGE_BYTES {
                return Err(Error::failed("download", "package exceeds limit"));
            }
            let path = stage.path().join(name);
            let mut file = File::create(&path).map_err(io_error)?;
            let limit = MAX_PACKAGE_BYTES.min(MAX_TOTAL_BYTES - total);
            let reply = response(
                self.http,
                asset_url(asset, self.source, self.version)?,
                false,
                limit,
            )?;
            total += copy_bounded(reply, &mut file, limit)?;
            file.sync_all().map_err(io_error)?;
            progress(host, self.id, self.version, "verifying_package", Some(name))?;
            manifest
                .verify_file(stage.path(), name)
                .map_err(verification_error)?;
            let package = self.package_metadata(&path, &arch)?;
            if !packages.insert(package.clone()) {
                return Err(Error::failed("release", "duplicate package identity"));
            }
            if let Some(current) = installed_package_version(host, &package)? {
                refuse_downgrade(host, self.version, &current, self.force)?;
            }
            paths.push(path);
        }
        if REQUIRED_PACKAGES
            .iter()
            .any(|name| !packages.contains(*name))
        {
            return Err(Error::failed(
                "release",
                "release must include control, privd, runtime and UI packages",
            ));
        }
        progress(host, self.id, self.version, "installing", None)?;
        // No shell, user argv or on-device build. Every path is a verified,
        // private file below this worker's staging directory. Nobody can answer
        // a conffile question here: an operator's edit of one (the list of
        // certified network backends) always wins over a changed default.
        let mut argv = args(&["--force-confdef", "--force-confold", "-i", "--"]);
        for path in paths {
            argv.push(
                path.to_str()
                    .ok_or_else(|| Error::failed("package", "staging path is not UTF-8"))?
                    .into(),
            );
        }
        let output = host.runner.run("/usr/bin/dpkg", &argv, None)?;
        host.root.atomic(
            "/etc/ndimon-firmware-version",
            format!("{}\n", self.version).as_bytes(),
            nix::unistd::getegid().as_raw(),
            0o644,
        )?;
        progress(host, self.id, self.version, "complete", None)?;
        Ok(output)
    }
    fn package_metadata(&self, path: &Path, arch: &str) -> Result<String, Error> {
        let path = path
            .to_str()
            .ok_or_else(|| Error::failed("package", "invalid staging path"))?;
        let mut values = Vec::new();
        for field in ["Package", "Version", "Architecture"] {
            let out = self.host.runner.run(
                "/usr/bin/dpkg-deb",
                &args(&["--field", path, field]),
                None,
            )?;
            values.push(out.stdout.trim().to_owned());
        }
        if !REQUIRED_PACKAGES.contains(&values[0].as_str())
            || values[1] != self.version.as_str()
            || ![arch, "all"].contains(&values[2].as_str())
        {
            return Err(Error::failed(
                "package",
                "unexpected package name, version or architecture",
            ));
        }
        Ok(values.remove(0))
    }
}

#[cfg(test)]
mod tests;
