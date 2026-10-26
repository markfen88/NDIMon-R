# Network profiles and VLAN assignments

Settings → Network defines the appliance networks on its primary wired port.
A network has a name, an untagged connection or VLAN ID (1–4094), and DHCP or
static IPv4 settings. Configure at most eight tagged VLANs and one untagged
network. Interface names are `<port>.<id>` and must fit Linux’s 15-character
limit. Untagged follows the switch port’s native VLAN; it does not imply VLAN 1.
Wi-Fi, bonds, bridges, IPv6 addressing and switch configuration are outside this
feature.

The network engine is part of `ndimon-privd`. It runs the same way on all four
supported backends (NetworkManager, Netplan with the networkd renderer,
systemd-networkd and ifupdown), keeps its state in a root-private journal, and
is driven from the web UI, the `/v1/Network` routes and the `stage_network`,
`confirm_network` and `revert_network` commands. Standalone Tessera has its own
network controls (networkd and Netplan); they refuse to change anything while
NDIMon-R owns the appliance, and updating Tessera alone does not replace an
installed NDIMon plugin bundle. See [runtime and plugin ownership](plugins/tessera.md#who-owns-what)
and [Ownership handoff](#ownership-handoff).

Configure the switch trunk to carry the selected VLAN IDs before applying them.
The list shows configured appliance networks; it does not discover VLANs from
the switch. Use distinct, non-overlapping IPv4 subnets. Only the management
network supplies a default gateway, DNS servers and search domains; DHCP on media
networks ignores router and DNS options. Static prefixes are 8–30, with up to
three IPv4 DNS servers. Duplicate static addresses are checked with ARP before
activation is accepted. An assigned network must obtain an IPv4 address before
confirmation.

## Assignments

- **Management / Control:** selected in Settings → Network. The web interface is
  available through this network. The selected network may be tagged or untagged.
- **NDI:** selected on the NDI page. This is one shared assignment for discovery
  and every decoder output. Independent networks per output remain deferred
  until receivers can run in separate processes.
- **Tessera:** selected inside the Tessera plugin. It uses the same configured
  network IDs. Tessera is pinned to that interface with a locked primary network;
  Dante device subscriptions continue to use the existing controller workflow.

Roles may share a network. The Network table shows these assignments as read-only
**Used by** links. Removing an assigned network is blocked: select replacement
networks in the corresponding control panels first, apply them, then remove the
unused network. Management can be reassigned within the Network draft. Changes to
an assigned network show a warning before updating the draft. There is no silent
reassignment to another VLAN when a selected network becomes unavailable. A saved
Tessera assignment is retained while the plugin is removed and restored when it
is installed/enabled.

## DNS search domains

The management network may carry up to six DNS search domains (host names: dot
separated labels of letters, digits and hyphens, at most 253 characters), with
DHCP or static addressing. Edit them in the **Add or edit a network** form while
the management network is selected. Other networks cannot have search domains.

The engine writes them into the host's own network configuration of the
management definition and takes them over from it when the device first adopts
it:

| Backend | Written and adopted as |
|---|---|
| Netplan | `nameservers.search` |
| NetworkManager | `ipv4.dns-search` (restored by Revert) |
| systemd-networkd | `Domains=` in `[Network]` |
| ifupdown | `dns-search` |

Domains supplied by a DHCP lease are never copied into the configuration, and
the engine never writes `UseDomains=`/`use-domains`. A configuration that uses
lease domains, routing-only (`~`) domains or an entry the row cannot hold is
refused as unrepresentable rather than trimmed. The device also accepts web
requests addressed as `hostname.<search domain>` (see [Security](../README.md#security)).

## Apply and recovery

Editing, adding and removing networks creates a browser draft. **Apply network
settings** previews the affected assignments and removals before making any
operating-system changes. Every apply is treated as potentially disruptive,
including a service assignment change, because backend reconfiguration may
interrupt management access. Running media services are stopped during the
change and restarted afterward; previously stopped services remain stopped.

The engine saves a root-owned write-ahead journal and arms a 180-second recovery
timer before it schedules the apply. The apply job has a 90-second runtime limit.
After the configuration has obtained its required addresses and the network
guards are installed, a separate **60-second** confirmation window starts.
Explicitly click **Keep changes**. Opening the link or refreshing the page does
not confirm. The confirmation dialog also works at the new address without the
previous login cookie; its random 64-character transaction token authorizes only
that pending change. **Keep**, **Revert**, Accept current network and the preview
wait up to three seconds for another network request that holds the engine's lock
(the page's own status poll does, for milliseconds) instead of answering 409
`network_operation`; the window's deadline is checked under the lock, so waiting
does not lengthen it. A lock that stays held (an apply in progress) is still refused.

**Revert**, an expired window, a failed apply, or boot recovery restores the
previous network files, NetworkManager settings, service bindings and firewall
rules. An unfinished recovery keeps its journal and retries every ten seconds.
**A recovery that cannot finish never leaves the appliance silent:** the media
services (decoder, finder, `tesserad`) are started again, a retry does not stop
them again, and the one that succeeds restarts them once on the restored network.
A banner on the Network card then says why the previous settings could not be
restored (the connection and the properties or files that differ). **Keep** and
**Revert** answer `409` (`apply_failed` at step `recover`, with the reason; it is
not a token failure, so it does not count toward the rate limit) and network
changes stay paused. If the cause cannot be removed, **Accept current network**
(the button in that banner, `POST /v1/Network/recovery/accept`, or the
`accept_network_recovery` command) is the way out. The device first shows what it found: the failure, what is live on
each network and any foreign configuration. Accepting then stops the retries,
puts the engine's own files and guards back to the committed network, removes the
VLANs and NetworkManager connections the failed change created and starts the
media services; the live addressing of the port is left as it is, and the
failure stays on record. A file the device cannot put back (an immutable engine
file, for example) is named in the answer (`warnings` and `unrestored`, and in the
page's notice): it still holds what the failed change wrote, and the next change
re-renders it. A handoff recovery cannot be accepted this way.
The same holds at boot: a recovery that fails there is reported by
`ndimon-net-boot` (exit 0, the journal says why) instead of failing the unit,
so the control plane, the finder and the decoder still start, with the committed
network's guards in place as far as they can be, and the retries continue.
The engine marks every managed link `RequiredForOnline=no` (an offline media
lease must not hold the boot hostage), so `network-online.target` does not wait
for the network selected for NDI. The decoder and the finder therefore wait for
that interface to get its address themselves, up to 60 seconds (the decoder
extends its start timeout while it waits), instead of exiting and being
restarted; a network that never gets an address is still refused, never replaced
by automatic selection.
For a NetworkManager profile that Netplan owns (Raspberry Pi OS, Ubuntu), the
restore puts back every file under `/etc/netplan`, `/run/netplan` and the two
`NetworkManager/system-connections` directories that names the profile, then
verifies the connection's addressing properties and compares the files again,
ignoring only the activation timestamp and `ipv6.ip6-privacy: "-1"` that
NetworkManager writes itself.
The timers are transient systemd units, so they run on the appliance
independently of the browser and of the control plane, and the
`ndimon-net-boot` unit re-establishes them after a reboot. The apply and
recovery jobs carry the transaction's public job identity, never its token, so
an old job cannot alter a newer change. Confirmation commits profiles and
assignments together with the document revision in one durable decision. Backups
restore networks and assignments through the same protected transaction, and the
other settings of the device cannot change while a network change is waiting.

For a DHCP management move, the new address appears after a lease is acquired.
If the old connection is lost before it can display that address, leave the
change unconfirmed and allow automatic recovery. Static management moves can be
prepared by placing the browser’s computer on the destination network first.

### Confirm policy

The profile document carries a `confirm_policy`:

| Policy | **Keep changes** is accepted |
|---|---|
| `token_only` (default) | From any address, with the token of the pending change. A change can be kept from a place that is unreachable from the new configuration, so keep it only when you are sure the device stays reachable. |
| `require_management_address` | Only when the request arrives on the new management address. |

Under both policies the token is required. The button is never disabled; if a
confirmation cannot succeed the device answers with a typed reason that the
dialog shows: `not_pending`, `expired`, `still_applying` (try again in a
moment), `token_mismatch` or `wrong_address`. Only a wrong token counts toward
the limit of five a minute per client, so pressing Keep while a change is still
being applied cannot lock you out. A recovery that has failed is answered with
`409` `apply_failed` (step `recover`) instead, and does not count either; see
[Apply and recovery](#apply-and-recovery) for **Accept current network**.

## Operating-system integration

The engine adopts the host's effective wired configuration on first use,
including the effective Netplan/networkd configuration rather than only files
previously created by NDIMon-R, and stores the complete profile document in the
device state. A root-private copy of the committed document and the journal are
under `/var/lib/ndimon-network` (root only). A public, root-owned interface
mapping lives at `/etc/ndimon-network-interfaces.json` for the native NDI
consumers.

- **systemd-networkd** uses exact interface matches in `00-ndimon-*.network` and
  `.netdev` files, which sort before the distribution's own files.
- **Netplan** resolves the existing Ethernet definition ID, preserves unrelated
  ports, journals the adopted IP fields so merged old address lists cannot stay
  active, and writes the same `00-ndimon-<iface>.network` pins so that a
  distribution file such as Armbian's `Name=e*` cannot give a static VLAN a DHCP
  address. The pins carry the route metric the backend would have used.
- **NetworkManager** reuses the base connection and owns its
  `ndimon-vlan-<interface>` connections.
- **ifupdown** needs VLAN support and `/etc/network/interfaces.d/`; it requires
  the ISC `dhclient` and refuses other DHCP clients.

**Armbian and Raspberry Pi OS.** Both ship profiles with settings that are not
address intent, and both are adopted:

- Armbian's `/etc/netplan/10-dhcp-all-interfaces.yaml` has `dhcp4`, `dhcp6` and
  `ipv6-privacy` on each of its three blocks (`e*`, `lan[0-9]*`, `wan[0-9]*`). The
  privacy switch (and its counterparts: networkd `IPv6PrivacyExtensions=`,
  NetworkManager `ipv6.ip6-privacy`, ifupdown `privext`) never counts as IPv6
  intent, and a bare `dhcp6: true` is the distribution's default, not a request for
  DHCPv6 addresses. Both are removed from the block that matches the managed port
  (the other two blocks are not touched), the managed configuration turns IPv6 off
  on its definitions as before, and Revert puts the file back byte for byte.
  `dhcp6-overrides`, `gateway6`, `accept-ra: true`, IPv6 addresses and IPv6 DNS
  still refuse.
- Raspberry Pi OS keeps wired profiles as Netplan-generated NetworkManager
  connections: a keyfile `netplan-eth0` in `/run/NetworkManager/system-connections`
  (no `interface-name`, `ipv4.route-metric` at `-1`, `ipv6.method auto`), and, once
  NetworkManager has saved a change, `/etc/netplan/90-NM-<uuid>.yaml` with
  `match: {}`. The engine treats such a profile as Netplan-owned: it does not write
  `connection.interface-name` (Netplan would drop it), it takes the metric `-1` as
  NetworkManager's Ethernet default of 100 and refuses it if the installed default
  route says otherwise, it reads the addressing back after writing it, and it
  records the profile's files before the change so that Revert restores them exactly
  (NetworkManager moves or rewrites them on every change). A profile file that holds
  credentials (802.1X, Wi-Fi security) is never copied: the change is refused
  (`unsafe_configuration`).

A distribution file that matches a managed interface is reported as drift.
Mixed DHCP/static overrides, multiple static addresses, custom static routes,
lease search domains and any other setting the profile model cannot express are
refused during adoption, as is a host managed by something else (dhcpcd, for
example). Software PTP was tested; hardware timestamping through VLAN interfaces
has not been validated.

**A refused adoption stays readable.** When the configuration cannot be adopted,
`GET /v1/Network/profiles` and `/v1/Network/status` still answer: they report the
refusal (`refusal` with the reason code and detail, naming the key and the file
and never the value) and the live address, and the Network page shows a read-only
list, a notice and disabled editors. Every change is refused with the same
reason until the configuration is brought within what the engine can represent.

### Backend validation

A network change is applied only for a backend that the host's administrator has
marked as validated. The engine reads the root-owned
`/etc/ndimon-network-certified-backends.json`, a JSON array of backend names
(`networkd`, `netplan`, `networkmanager`, `ifupdown`).

The 1.4.0 packages list **all four**: each passed the apply, keep, revert, expiry
and reboot scenarios with the real tools (not mocks) on disposable guests, and 1.3.x
could change VLANs on all of them. The file is a Debian conffile of `ndimon-privd`.
A fresh install and the upgrade of a 1.3.x appliance create it; an upgrade keeps
your edit of it, and a file you deleted is not restored. The update job and
`cargo xtask install` run dpkg with `--force-confdef --force-confold`, so they never
stop at a question about it. If you wrote the file by hand before the first package
install, install with `-o Dpkg::Options::=--force-confold` too: without it dpkg asks
whether to replace your file and fails when nobody can answer.

Withdraw a backend you have not validated on your own hardware and switch by
editing the list, for example `["networkd"]`. For a backend that is not listed the
status and preview still work, but staging a change (and a handoff) is refused with
`unsupported` / `backend_not_certified`, the web page is read-only and the backup
review marks the network section as not applicable. A change that was staged
before the list changed is refused when its apply job starts.

The file must be a regular file owned by root that others cannot write. If it is
missing, unreadable or not a JSON array of those four names (an unknown name
counts), no backend is validated: changes are refused as above, the status and
preview keep working, the boot job still runs (the stack does not depend on it),
and `ndimon-privd` logs why once to the journal (`journalctl -u ndimon-privd`).
Restore it by writing the names back or reinstalling the `ndimon-privd` package
with `dpkg --force-confmiss`. Add a backend that a later release supports only
after you have run the same cases on the same kind of host and switch.

### Isolation

The engine owns one nftables table, `inet ndimon_management`. It limits web access
to the management network, blocks forwarding between the configured networks,
and restricts NDI and Tessera service traffic to their selected interfaces plus
loopback. Stable systemd slices (`ndimonndi.slice`, `ndimontessera.slice`)
identify the services; the rules use nftables socket/cgroup matches and do not
depend on systemd’s optional BPF framework. Boot recovery rebuilds the rules
before the services that need them start. The native NDI processes also write the
selected IPv4 address to the SDK adapter allowlist and restart SDK
initialization when the interface or address changes. Tessera receives an
explicit, locked interface configuration.

## Ownership handoff

When NDIMon-R hosts Tessera it owns the network, and standalone Tessera refuses
to change it. To move a device between the two products, the products exchange a
root-private handoff record on the same host. It carries the intended networks,
the backend and adoption metadata, and hashes of the files involved, never
configuration text or another product's journal. Both products fence themselves
while a handoff is open, and the receiving engine applies the change through its
ordinary journal, so the usual 60-second Keep/Revert window and recovery apply.

| Step | Route | Command |
|---|---|---|
| Release this device's network to the other product | `POST /v1/Network/handoff/prepare` (and `/cancel`) | `prepare_network_handoff`, `cancel_network_handoff` |
| Take over from the other product | `POST /v1/Network/handoff/import`, then `/commit` | `import_network_handoff`, `commit_network_handoff` |

`import` reads the other product's record and returns an impact whose token you
send as `expect_impact` on `commit`. `commit` returns the usual pending-change
token and does not keep the change by itself. Until you Keep, the previous owner
owns the configuration; after Keep the receiving product does, and the previous
owner is archived. A refused handoff reports a drift when a file that the record
hashes has changed. The handoff needs the same backend validation as any change.

## API

Authenticated endpoints:

- `GET /v1/Network/profiles`: committed settings, live network status, assignments,
  any pending transaction and, when the configuration could not be adopted, the
  `refusal`.
- `POST /v1/Network/profiles/preview`: validate a complete candidate document and
  report affected assignments/removals without changing the OS.
- `POST /v1/Network/profiles/apply`: stage the complete candidate document. Its
  revision must match the current revision.
- `POST /v1/Network/profiles/rollback`: restore the previous configuration.
- `POST /v1/Network/recovery/accept`: give up a recovery that cannot finish and keep the network as it is; the first call answers `confirmation_required` with what the device found, repeat it with `expect_impact` (see [Apply and recovery](#apply-and-recovery)).
- `POST /v1/Network/handoff/{prepare,cancel,import,commit}`: see above.

The same operations are available as the typed `stage_network`, `confirm_network`,
`revert_network`, `rollback_network` and handoff commands on `POST /api/v1/commands`.

Confirmation endpoints accept the transaction token without an old login cookie:

- `POST /api/net-pending`: `{ "token": "<64 hex characters>" }`.
- `POST /api/net-confirm`: `{ "token": "...", "profiles": true }`.
- `POST /api/net-revert`: `{ "token": "..." }`.

The document contains the fixed physical `interface` and `backend`, a `networks`
array, the network IDs for `management`, `ndi` and `tessera`, and the
`confirm_policy`. Each row contains `id`, `name`, `vlan_id` (null for untagged),
`mode`, `address`, `prefix`, `gateway`, `dns`, and optionally `dns_search`
(management only, at most six host names, left out when empty). The control
plane validates its shape and the privileged engine validates all settings and
assignments again. Independent `PUT`/`DELETE /v1/Network/vlans/:id` return a
refusal; callers must use the complete transaction.

Passing automated scenarios does not certify a physical switch, a hardware
platform or hardware PTP through VLAN interfaces. Run a reboot and power-cut test
on a dedicated appliance with a configured switch trunk before relying on a
network change in the field.
