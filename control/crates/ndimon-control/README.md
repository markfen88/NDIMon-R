# ndimon-control

The Rust front door of the appliance. It owns HTTP, the static UI, authentication,
the Host allowlist, the legacy `/v1` compatibility routes, monitoring, server-sent
events and the source/health supervisor, and it starts the state actor
([`ndimon-state`](../ndimon-state/README.md)) that every write goes through. No
Node.js process exists: nothing is proxied and there is no private listener or
internal socket. A protected path that no native router claims answers a local 404.

`EventHub::publish` accepts `ndimon_types::Event`. The hub broadcasts shared typed
events and the HTTP boundary serializes them with the public wire contract.
Production authentication writes use `ndimon-privd-proto` with its `client`
feature.

## Packaged installation

Install all four packages produced by `cargo xtask build-deb`, or run
`sudo -E cargo xtask install` from `control/` on a prepared appliance checkout (see
[the workspace README](../../README.md)). The native package hooks stop the writers,
preserve manual units, import the state and migrate network storage before they
activate the stack, and they stop, disable and remove the retired standalone
watchdog. There is no script installer and no Node or sudo rollback mode.

The packages install control under `/usr/bin`, privd under `/usr/libexec/ndimon`
and the public assets under `/usr/share/ndimon/public`. Tessera's manifest and page
fragment are under `/usr/share/ndimon/plugins/tessera`; an installed plugin bundle
takes precedence. Mutable plugin bundles live under `/opt/ndimon-r/plugins`.
Consolidated tmpfiles declarations own the private runtime directories and the
shared state parents; no unit uses `StateDirectory=` on those trees. Authentication
writes require privd.

The socket unit binds `0.0.0.0:80` and `[::]:80` with `BindIPv6Only=ipv6-only`, so
the two listeners never overlap, and the process takes every activated listener
with `listenfd`. It reports `READY=1` once the state actor has recovered and
rendered, sends `WATCHDOG=1` at half the configured systemd watchdog interval, and
on shutdown drains requests for up to 5 seconds while the actor finishes or rolls
back a transaction in flight (up to 8 seconds).

## Configuration

```text
--listen IP:PORT        fallback without socket activation (127.0.0.1:8080)
--public-dir PATH       /usr/share/ndimon/public
--auth-file PATH        /etc/ndimon-auth.json
--decoder-socket PATH   /tmp/ndi-decoder.sock
--finder-socket PATH    /run/ndimon/finder.sock
--tessera-socket PATH   /run/tessera/control.sock
--state-dir PATH        /var/lib/ndimon/state
--legacy-root PATH      /
--privd-socket PATH     /run/ndimon-privd/control.sock
--cookie-secure         add Secure to the session cookie when served over HTTPS
```

The service runs as `ndimon-control:ndimon-control` with the supplementary groups
`ndimon` (read the auth record, traverse the shared state parents),
`ndimon-priv-control` (connect to privd's control socket, which is how privd
identifies the caller) and `ndimon-finder` (the finder's scan trigger file).
`NoNewPrivileges=yes` and `ProtectSystem=strict` apply; the only writable paths are
`/run/ndimon-control`, the state, import, backup and asset directories, `/tmp` and
the finder trigger file. Generated `/etc` files and the auth record are written by
privd, not by this process. `PrivateTmp=no` keeps the decoder's
`/tmp/ndi-decoder.sock` visible. `ndimon-state-import` is a separate root oneshot
that runs before the service.

An alternate `--auth-file` is for development and tests: its parent must already
exist, and the record is replaced atomically with mode 0640 and file and directory
fsync. Only the production path `/etc/ndimon-auth.json` is written through
`Request::WriteLegacyConfig { file: Auth, json }` on the privd control socket,
bounded by a 10-second deadline; privd replaces the file atomically as
`root:ndimon` 0640. No password or hash is logged or passed as an argument.

## State, commands and native routes

Startup builds a `ProductionApplier` (framed privd client, decoder and finder
IPC, Tessera client), then `StateHandle::start` recovers an interrupted
transaction and renders the boot artifacts before any listener opens. The replay,
follower and observer tasks and the event relays are aborted on shutdown.

| Route | Purpose |
|---|---|
| `GET /api/v1/state`, `/api/v1/state/{domain}` | Desired document (or one domain) with `revision` and `domain_revision` |
| `GET /api/v1/observed`, `/api/v1/status`, `/api/v1/catalog/{name}` | Observed state with drift, aggregate status, editor catalogs |
| `POST /api/v1/commands` | `{command, expect_revision?, expect_impact?, confirm?}` → `{ok, revision, report, result}` |
| `GET /api/v1/events` | Server-sent events (below) |
| `GET/POST/DELETE /api/v1/session`, `POST /api/v1/session/password` | Sign in, sign out, change password |

The UI origin of a command comes from the authenticated session's separate public
ID; no credential appears in events. The same router mounts
`ndimon_compat::legacy_router`, which serves `/v1/*` (audio, outputs, NDI, device,
system, presets, display, backup, network, plugins, Tessera) and
`/api/v1/system/*` under the same authentication, CSRF, Host and Origin middleware,
with a 4 MiB JSON body limit. Legacy settings writes may carry `confirm` and
`expect_impact`; these are request controls and never become saved fields. Plugin
archive uploads stream natively with their own 200 MiB limit. The monitoring routes
`/api/health`, `/api/status`, `/api/services`, `/api/services/{name}/restart`,
`/api/watchdog-stats` and `/api/events` are served by this crate.

## Authentication

`POST /api/v1/session` takes `{"password": "..."}` (bodies are limited to 16 KiB)
and returns `{"csrf": "..."}` with the `ndimon_session` cookie; `DELETE` expires
it and answers 204. Sessions are random 256-bit tokens with a random per-session
CSRF token. The cookie is `HttpOnly; SameSite=Strict; Path=/` (plus `Secure` with
`--cookie-secure`). A session expires after 8 idle hours, the table is memory-only
and bounded to 64 entries, and it is bound to the digest of the auth file, so
changing or restoring the password record ends every earlier session on its next
request.

| Endpoint | Response |
|---|---|
| `GET /api/v1/session` | `{logged_in: false}` without a session; `{logged_in: true, csrf, default_password}` with one |
| `POST /api/v1/session/password` `{current_password, new_password}` | `{csrf}` and a replacement cookie; every older session is dropped. A wrong current password is `permission_denied` and counts as a failed login; a new password under 6 characters is `invalid` on `new_password` |

The earlier routes remain with their original shapes plus an additive `csrf`:
`POST /api/login` → `{ok, token, default_password, csrf}`, `GET /api/auth-status`
→ `{auth_required, logged_in, default_password, csrf?}`, `POST /api/logout`, and
`POST /api/password` `{current, password}`.

Every cookie-authenticated `POST`, `PUT`, `PATCH` and `DELETE`, native or legacy,
must send the session's `X-CSRF-Token`; after a password change use the returned
replacement token. Bearer credentials are the same session tokens, need no CSRF
token and take precedence over a cookie. A mutation that carries an `Origin` must
have an origin that matches `Host`, bearer requests and login included.
Unauthenticated routes are only login, the auth-status probes, loopback
`GET /api/health` and the three token-only network POST endpoints. Forwarding
headers never establish loopback trust, and case, percent-encoding and doubled
slashes cannot spell a protected path around authentication.

**Login limiter.** Failures are counted per source (an IPv4 address, or an IPv6
/64; a mapped IPv4 address shares its IPv4 bucket) over a sliding 60-second window.
From the fifth failure a source is refused with 429 `rate_limited` and a
`Retry-After` that starts at 1 second and doubles with every further failure up to
30 seconds, measured from its last failure. A correct password does not skip the
wait; a success clears the source's count. Fifty failures from all sources inside
the window never refuse anybody: they only add a delay of 250 ms rising to 2 s.
At most 4,096 source buckets are kept. Password changes use the same accounting.

**Stored password.** With no auth file (or an empty one) the password is `ndimon`
and the UI keeps its warning until the operator changes it. A 1.3.x record has a
16-byte hex salt and a 32-byte hex scrypt key (`N=16384, r=8, p=1`). A successful
login adds `algo: "argon2id"` and a `phc` string (the Argon2 crate's defaults); a
password change writes both formats so a rollback to 1.3.x keeps working. A corrupt
or unreadable non-empty record fails closed, and imported PHC work factors are
bounded (`m` ≤ 65536, `t` ≤ 10, `p` ≤ 8). Backup export and restore carry the
algorithm and PHC fields and still accept old salt/hash-only records.

## Host allowlist

On every protected path, before authentication, exactly one `Host` authority must
name this device, or the answer is HTTP 421 `misdirected_host` with no redirect.
This closes DNS rebinding. The accepted names are read per request (so an address
added by a network change counts at once): `localhost` and loopback addresses, any
address of any interface (IPv6 literals in brackets, optional numeric port), the
kernel hostname and `<hostname>.local`, `<hostname>.<domain>` for each domain of
the effective resolver search list (`/etc/resolv.conf`, or systemd-resolved's
generated file when the device uses it; route-only domains are ignored), and up to
16 exact DNS names the operator saved as `device.web_allowed_hosts` (Settings →
Security → Additional host names; no wildcards or IP literals). No DNS answer and
no forwarded header can widen the list. The reflected host in the error is stripped
of control characters and capped at 253 characters.

## Events and HTTP policy

`GET /api/v1/events` requires a session. Frames are `event: event` with one
externally tagged JSON event in `data`, `event: resync` with empty data after
lag or a decoder reconnect (the client re-reads state), and a keep-alive comment
every 15 seconds. The decoder follower sends `{"action":"subscribe"}\n`, reads
bounded JSON lines and publishes `{"status":{"source":"decoder","data":...}}`; it
also samples the decoder's `status` and reads the finder's source list every 1.5
seconds for all browser tabs, and reconnects with a backoff of 1 to 30 seconds. State
events (`state_changed`, `apply_progress`, `observed`, `drift`) come from the actor.
`StatusSource` is extensible (`decoder`, `finder`, `tessera`, `network`, `services`);
status data keeps the source's JSON and is `unknown` in TypeScript.

Errors use the shared envelope `{"error": {"<kind>": {...}}, "message": ...}`.
Every response carries `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer` and
`X-Content-Type-Options: nosniff`. The default cache policy is `no-store`; static
HTML always uses it, other public assets are cached for a day, and the SSE streams
use `no-cache`. There is no Content-Security-Policy, because the page still has
inline scripts.

## Supervisor

`supervisor` runs source recovery and the health policy; every effect enters the
state queue. It polls the decoder's `status` every 5 seconds and `health` every 15
seconds and also reacts to decoder connection events. A disconnected output with a
saved source is retried with a backoff of 5, 10, 20 and then 30 seconds, using the
saved address on odd tries and the name alone on even ones, and a user's connect
gets 30 seconds of grace. Each retry is a `RetrySource` command that connects the
saved source without changing saved intent. In `watchdog_mode = active` an output
whose `stall_count` reaches 60 (about 30 s) gets the same retry cycle; `passive`
only counts and `disabled` skips the poll. Counters are published to `/tmp/ndimon-watchdog-stats.json`
for `/api/watchdog-stats`. Process liveness belongs to systemd.

## Development

The `dev-tools` feature builds a fixture binary that runs the production actor and
applier against a temporary tree and fake effects. The custom state, root and
socket flags permit a non-root run; never use them to bypass production
ownership. Typical checks, from `control/`:

```text
cargo fmt --all --check
cargo clippy -p ndimon-control -p ndimon-types --all-targets --all-features -- -D warnings
cargo check -p ndimon-types --no-default-features
cargo xtask export-types
```
