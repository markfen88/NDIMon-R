# Network profiles and VLAN assignments

Settings → Network defines the appliance networks on its primary wired port.
A network has a name, an untagged connection or VLAN ID (1–4094), and DHCP or
static IPv4 settings. Configure at most eight tagged VLANs and one untagged
network. Interface names are `<port>.<id>` and must fit Linux’s 15-character
limit. Untagged follows the switch port’s native VLAN; it does not imply VLAN 1.
Wi-Fi and switch configuration are outside this feature.

NDIMon manages networking when it hosts Tessera. Standalone Tessera networking
is configured separately; do not let two managers own the same port.

Configure the switch trunk to carry the selected VLAN IDs before applying them.
The list shows configured appliance networks; it does not discover VLANs from
the switch. Use distinct, non-overlapping IPv4 subnets. Only the management
network supplies a default gateway and DNS; DHCP on media networks ignores
router and DNS options. Static prefixes are 8–30, with up to three IPv4 DNS
servers. Duplicate static addresses are checked with ARP before activation is
accepted. An assigned network must obtain an IPv4 address before confirmation.

## Assignments

- **Management / Control:** selected in Settings → Network. The web interface is
  available through this network. The selected network may be tagged or untagged.
- **NDI:** selected on the NDI page. This is one shared assignment for discovery
  and every decoder output. Independent networks per output remain deferred
  until receivers can run in separate processes.
- **Tessera:** selected inside the Tessera plugin. It uses the same configured
  network IDs. Tessera is pinned to that interface with a locked primary network;
  Dante device subscriptions continue to use the existing controller workflow.

The Network table shows these assignments as read-only **Used by** links. Removing
an assigned network is blocked: select replacement networks in the corresponding
control panels first, apply them, then remove the unused network. Management can
be reassigned within the Network draft. Changes to an assigned network show a
warning before updating the draft. There is no silent reassignment to another
VLAN when a selected network becomes unavailable. A saved Tessera assignment is
retained while the plugin is removed and restored when it is installed/enabled.

## Apply and recovery

Editing, adding and removing networks creates a browser draft. **Apply network
settings** previews the affected assignments and removals before making any
operating-system changes. Every apply is treated as potentially disruptive,
including a service assignment change, because backend reconfiguration may
interrupt management access. Running media services are stopped during the
change and restarted afterward; previously stopped services remain stopped.

The privileged helper saves a root-owned write-ahead journal and arms an initial
150-second recovery timer before scheduling the apply. The apply job has a
90-second runtime limit. After the configuration has obtained its required
addresses and the network guards are installed, a separate **60-second**
confirmation window starts. Open the displayed management address and explicitly
click **Keep changes**. Opening the link or refreshing the page does not confirm.
The confirmation dialog also works at the new address without the previous
login cookie; its random transaction token authorizes only that pending change.
The backend verifies the actual local socket address, rather than forwarded
headers, before accepting confirmation.

**Revert**, an expired timer, a failed apply, or boot recovery restores the
previous network files, NetworkManager settings, service bindings and firewall
rules. An unfinished recovery keeps its journal for another attempt. The timer
runs on the appliance independently of the browser and API process. Old apply
and rollback jobs carry transaction tokens so they cannot alter a newer change.
Confirmation commits profiles and assignments together. Backups restore networks
and assignments through the same protected transaction, rather than separate
immediate VLAN writes.

For a DHCP management move, the new address appears after a lease is acquired.
If the old connection is lost before it can display that address, leave the
change unconfirmed and allow automatic recovery. Static management moves can be
prepared by placing the browser’s computer on the destination network first.

## Operating-system integration

NetworkManager, Netplan, systemd-networkd and ifupdown adapters are implemented.
Unknown backends are refused. The helper adopts the current wired configuration
on first use, including the effective Netplan/networkd configuration rather than
only files previously created by NDIMon. It stores the complete profile document
privately under
`/var/lib/ndimon-network` (root only). A public, root-owned interface mapping lives
at `/etc/ndimon-network-interfaces.json` for the native NDI consumers.

Networkd uses exact interface matches in `00-ndimon-*.network` and `.netdev`
files. Netplan resolves the existing Ethernet definition ID, preserves unrelated
ports, and journals adopted IP fields so merged old address lists cannot remain
active. NetworkManager reuses the base connection and owns its
`ndimon-vlan-<interface>` connections. Ifupdown requires support for VLANs and
`/etc/network/interfaces.d/`; DHCP suppression on media networks requires a client
that honors dhclient enter hooks.

The installer includes nftables. The helper owns only the
`inet ndimon_management` table: it limits web access to management, blocks
forwarding between the configured interfaces, and restricts NDI/Tessera service
traffic to their selected interfaces plus loopback. Stable systemd slices identify
the services. These nftables socket/cgroup rules do not depend on systemd’s optional BPF framework. Boot recovery rebuilds
these rules before services that require them start. The native NDI processes
also write the selected IPv4 address to the SDK adapter allowlist and restart SDK
initialization when the interface/address changes. Tessera receives an explicit,
locked interface configuration. Mixed DHCP/static overrides, multiple static addresses and custom static routes
that the profile model cannot express are refused during adoption.
Hardware PTP timestamping through VLAN interfaces is not certified.

## API

Authenticated endpoints:

- `GET /v1/Network/profiles`: committed settings, live network status, assignments
  and any pending transaction.
- `POST /v1/Network/profiles/preview`: validate a complete candidate document and
  report affected assignments/removals without changing the OS.
- `POST /v1/Network/profiles/apply`: stage the complete candidate document. Its
  revision must match the current revision.
- `POST /v1/Network/profiles/rollback`: restore the previous configuration.

Confirmation endpoints accept the transaction token without an old login cookie:

- `POST /api/net-pending`: `{ "token": "<64 hex characters>" }`.
- `POST /api/net-confirm`: `{ "token": "...", "profiles": true }`.
- `POST /api/net-revert`: `{ "token": "..." }`.

The document contains `version: 1`, `revision`, the fixed physical `interface`
and `backend`, a `networks` array, and `management`, `ndi`, `tessera` network IDs.
Each row contains `id`, `name`, `vlan_id` (null for untagged), `mode`, `address`,
`prefix`, `gateway`, and `dns`. The API validates its shape and the root helper
validates all settings and assignments again. Existing status/backup reads remain
available. Independent `PUT`/`DELETE /v1/Network/vlans/:id` and immediate privileged
VLAN mutation commands return a refusal; callers must use the complete transaction.
