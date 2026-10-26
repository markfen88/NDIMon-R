# ndimon-tessera

NDIMon-R's own client for the control socket of Tessera's audio daemon
(`/run/tessera/control.sock`). It implements only the version-1 framing and the
subset of Tessera's control types that NDIMon-R uses; no Tessera crate is linked, and
the two projects share no code. NDIMon-R 1.4.0 works with Tessera 0.3.0 or later. The
tested control-type release is recorded in `compatibility.json` at the repository
root.

## Transport

A frame is a big-endian `u32` length followed by UTF-8 JSON, 1 byte to 1 MiB
(`MAX_FRAME`). Requests carry `{v: 1, id, ...}` and the reply must repeat the version
and id (an id-0 error is accepted for a rejected frame). `Client::new(path, timeout)`
keeps one sequential session: a request that times out, fails to frame or loses the
socket drops the session and the next request reconnects, but a mutation is never
retried implicitly. `Client::default()` uses `/run/tessera/control.sock` and 3
seconds. `online()` reports whether the last exchange worked.

Events use an independent connection: `subscribe()` sends `SubscribeEvents`,
requires the acknowledgement and returns a `Subscription` whose `next()` yields
`Event`s or a `Lagged` notice. An idle subscription has no deadline; a partial frame
has 10 seconds, so periodic status polling cannot cancel a half-read frame.

## API

- `status()`, `metrics()` and `request(RequestBody)` for queries. Observations keep
  the daemon's extra fields, and unknown event variants are tolerated.
- `command(Command, confirmation)` where `confirmation` is `None` (unconfirmed),
  `Some(None)` (blind confirmation) or `Some(Some(token))`, which forwards exactly the
  opaque impact token the operator saw. The supported commands cover the device name,
  sample rate, encoding, latency, channel counts and labels, clock role and priority,
  and reload.
- `Status::supports_flow_lock()` is true when the daemon reports version 0.2.2 or
  later, the first to honour `[control] locked = ["flows"]`. For an older daemon the
  state crate omits that setting and reports an ownership conflict instead of
  failing (an older runtime cannot stop a controller from changing runtime flows).
- Errors are `Io`, `Timeout`, `Protocol` or `Command(CommandError)`; refusals such as
  a locked value, a required confirmation or a rate limit stay typed.

NDIMon-R never sends the controller flow command: its host flows are rendered
configuration and protocol writes to them are locked.

## Boundaries

The state crate owns when and why to call this client (apply hooks, replay, the
observer). Nothing here writes files; the Tessera configuration under
`/etc/tessera/config.d` is rendered by `ndimon-render` and published by
`ndimon-privd`.
