# M0 — Foundations

**Status: done (2026-10-08).** All exit criteria met. (Postgres and Valkey, part of this milestone's skeleton and CI,
were replaced during M1's decisions: SQLite for the database, memory for temporary data.)

Everything the later milestones stand on: the workspace, CI, the packet layer, a recorded corpus of real
game sessions, and a replay harness proven against Python MH3SP. No game logic yet.

## Exit criteria

1. CI (Woodpecker `push`) is green: fmt, clippy `-D warnings`, tests, with Postgres and Valkey services.
2. The corpus holds the six scenarios below (two of them recorded twice), imported and committed.
3. Every recorded TCP stream re-encodes byte for byte through `wire` (framing round-trip on 100 % of frames).
4. The replay harness replays every scenario against Python MH3SP (pinned commit) with **zero unexplained diffs**.
   This proves the harness, the importer and the masks before any Rust game code exists.
5. The Rust server skeleton starts, connects to Postgres and Valkey, accepts the game's connections (TLS on OPN)
   and logs every frame by name. The harness runs against it and reports the expected failures without
   breaking CI.

## Workspace

```
mh3sp-rs/
├── Cargo.toml              # workspace
├── rust-toolchain.toml     # pinned stable toolchain
├── config.example.toml
├── certs/dev/              # MH3SP's dev certificate + key (signed by the CA the patched game trusts)
├── migrations/             # sqlx migrations (empty in M0; accounts arrive in M1)
├── corpus/                 # recorded scenarios (see Corpus)
├── crates/
│   ├── wire/               # packet layer: framing, ids, items, PatData. No game knowledge, no I/O
│   ├── server/             # the `mh3sp` binary: config, listeners, TLS, DB, Valkey (game logic from M1)
│   └── testkit/            # scripted console client, corpus format + importer, replay harness
│       └── src/bin/corpus.rs   # `corpus import|diff|replay`
├── tools/
│   ├── gen_catalogue.py    # regenerates crates/wire/src/catalogue.rs from MH3SP + MHTrIDA
│   └── record-scenario.sh  # drives a recording with the lab (see Recording)
└── .woodpecker/push.yml
```

Why three crates: `server` and `testkit` both need `wire`; the test client must never ship in the server binary;
and `testkit` can dev-depend on `server` to start it inside tests without a dependency cycle.

### Dependencies

| Crate | Uses |
|---|---|
| `wire` | `bytes`, `thiserror`, `tokio-util` (codec only); `proptest` (dev) |
| `server` | `tokio`, `tokio-util`, `tokio-openssl` + `openssl`, `sqlx` (postgres, runtime-tokio, macros, migrate), `redis` (tokio-comp, connection-manager), `serde` + `toml`, `tracing` + `tracing-subscriber` |
| `testkit` | `wire`, `tokio`, `tokio-openssl`, `serde` + `serde_json` + `toml`; pcap parsed by hand (the format is ~40 lines; no crate) |

Edition 2024, latest stable toolchain pinned in `rust-toolchain.toml`.

## 1. Workspace and CI

- [x] Cargo workspace (`crates/*`), `rust-toolchain.toml`. Crates join when their step starts: `wire` now,
      `testkit` at step 3, `server` (+ `config.example.toml`) at step 7.
- [x] `scripts/dev.sh`: starts compose, creates/completes `.env` (idempotent, keeps existing values).
- [x] `scripts/ci-tests.sh`: what CI runs, natively, `--docker` in the CI image, or `--woodpecker` (the pipeline
      files through the local runner); CI calls this same script, with `CARGO_TARGET_DIR=target/ci`.
- [x] `.woodpecker/push.yml` (calls `scripts/ci-tests.sh`):
  - image `rust:<pinned>-bookworm` (has `libssl-dev`), `rustup component add rustfmt clippy`;
  - steps: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`;
  - services: `postgres:17` (user/password/db `mh3sp`) and `valkey/valkey:8`;
  - env: `DATABASE_URL=postgres://mh3sp:mh3sp@postgres:5432/mh3sp`, `VALKEY_URL=redis://valkey:6379`,
    `SQLX_OFFLINE=true`;
  - no build cache in M0; add one (agent volume) only if pipelines get slow.
- [ ] `cargo sqlx prepare --check` step as soon as the first `query!` exists (M1).

## 2. Server skeleton (`crates/server`)

Only what M1 builds on; nothing here gets thrown away.

- [x] `mh3sp [config.toml]` (default `config.toml`). Config: listeners (`opn`, `lmp`, `fmp`: bind address, absent
      = disabled; `health`), TLS cert/key, `database.url`, `valkey.url`; unknown keys are rejected.
      `DATABASE_URL` / `VALKEY_URL` env vars override the file. `external_address` arrives with `AnsLmpConnect` (M1).
- [x] Startup: connect Postgres (pool) and run `sqlx::migrate!()`; connect Valkey (connection manager) and `PING`.
      Either unreachable → exit 1 with a clear error.
- [x] Listeners: one tokio task per listener, one task per connection. OPN is TLS through `tokio-openssl` with the
      legacy setup (decision 6 in `00. global.md`): `@SECLEVEL=0`, minimum protocol SSLv3/TLS 1.0, ciphers
      `AES128-SHA:AES256-SHA:DES-CBC-SHA:DES-CBC3-SHA:RC4-MD5:RC4-SHA` (same list as MH3SP `other/utils.py`).
- [x] Logging per decision 8 of `00. global.md` (colours only on a terminal): `tracing-subscriber` terminal output with `EnvFilter`, one span per
      connection (`conn`, `role`); anonymous (no names, no IPs) and no secrets.
- [x] Each connection: read frames with the `wire` codec, log one line per frame (`RECV <name> id seq len`, hexdump at
      `debug`), keep the connection open until the peer closes it. No replies yet (M1 adds `ReqConnection`).
- [x] Graceful shutdown on Ctrl-C and SIGTERM (`docker stop`): stop accepting, close connections.
- [x] Health endpoints and `mh3sp healthcheck` (decision 9 of `00. global.md`). Verified: Valkey or Postgres
      stopped → live 200, ready 503 with the reason, back to ready by itself when they return.
- [x] `Dockerfile` (+ `.dockerignore`): verified healthy under Docker, Wii TLS handshake with Debian's OpenSSL,
      `docker stop` exits 0.
- [x] `certs/dev/`: copy of MH3SP `cert/server.crt` + `server.key` (+ `ca.der` for reference).

## 3. Packet layer (`crates/wire`)

Facts checked against MH3SP `dev` and MHTrIDA:

- **Frame header**, 8 bytes, big-endian: `size: u16` (payload length), `seq: u16`, `id: u32`. Payload ≤ 65535 bytes.
- **Packet IDs**: `0xCCNNKK00`: `CC` category (`0x60` Opn, `0x61` Rfp, `0x62` Lmp, `0x63` Fmp, `0x64` Layer,
  `0x65` Circle, `0x66` User, `0x69` Agreement), `NN` message, `KK` kind (`01` Req, `02` Ans, `10` Ntc). The low
  byte is always `00`: MH3SP and MHTrIDA list the **same 301 IDs**, none with a non-zero low byte.
- **Items** (tag byte + value): `1` Byte (`u8`), `2` Word (`u16`), `3` Long (`u32`), `4` LongLong (`u64`),
  `5` String (`u16` length + bytes), `6` Binary (`u16` length + bytes). MH3SP's enum also has `0` Custom and
  `7` Object; neither is used (see the quirk below).
- **Strings are bytes**, not UTF-8: names and texts can be Shift-JIS. Keep them as `Bytes` everywhere.
- **PatData**: `u8 count`, then `count × (u8 field_id, item)`. Field **order matters** for byte equality, so it is
  stored in order (`Vec`), never a map. Some answers only contain the fields the client asked for: the request
  carries a byte list (`u8 count` + field ids), see MH3SP `pack_fields` / `AnsFmpInfo`.
- Other primitives: `lp_string` (`u8` length), `lp2_string` (`u16` length), byte lists (`u8` count + bytes).
- **Quirk (historical)**: MH3SP once worked around the game mis-parsing `LayerData` field `0x17` with a dummy
  `Custom` field (`FallthroughBug`); on `dev` the workaround is commented out, so `wire` has no `Custom` item.
  To re-check against the corpus when `LayerData` is implemented (M2).

Tasks:
- [x] `Frame { seq, id, payload }` + `FrameCodec` (`Decoder`/`Encoder`): partial frames, several frames per read,
      oversize payload → error.
- [x] `PacketId(u32)` with `category()`, `kind()`, `name()`, `entry()`.
- [x] `catalogue.rs`: the 301 IDs with name, Japanese name and English description, generated by
      `tools/gen_catalogue.py` from MH3SP `PatID4` and MHTrIDA `server/doc/PAT.MD`, committed.
      Findings: MH3SP's `PAT_NAMES` reuses 12 names (the LMP- and FMP-stage server list packets, both ticket
      packets), so names come from `PatID4`, which is unique (`ReqFmpInfo` = 61340100, `ReqFmpInfo2` = 63140100).
      Two ids, 64160200 and 64170200, carry the Ans kind byte but the game itself names them
      `NtcLayerJumpReady`/`NtcLayerJumpGo` (described as "reply"): kept as the game names them.
- [x] `Item`, `PatData` (ordered fields, `select(&fields)`), `OptionalFields`, `lp_string`, `lp2_string`, byte
      lists (`wire/src/pat.rs`): a `Reader` cursor whose errors carry the offset (truncated, unknown item type,
      trailing bytes), and a `PatWrite` trait on `BytesMut`/`Vec<u8>` whose length prefixes can't overflow silently.
- [ ] Typed packet structs: **not in M0**. Each one arrives with the handler that needs it (M1+), so its shape is
      checked against the corpus at the same time.

Tests (tier 1):
- [x] Unit tests for every primitive, including truncated input and oversize length.
- [x] `proptest`: `decode(encode(x)) == x` for `PatData` and `OptionalFields`, and decoding random bytes never panics.
- [x] Corpus test (`testkit/tests/corpus_roundtrip.rs`): for every recorded connection and direction, decoding the
      stream into frames and re-encoding gives back the exact bytes; committed `tcp/`/`udp/` files must match what
      `raw/` regenerates (stale files fail). `CORPUS_DIR` points it at another corpus.
- [x] `PatData` round-trip on real payloads known to be PatData: `AnsConnection` (`ConnectionData`, whole payload)
      and `ReqCircleCreate` (`CircleInfo` prefix), 30 payloads. `NtcCollectionLog` joins when a recording has one.

## 4. Corpus

### Source: Dolphin's captures

Verified on today's lab captures: with `DumpAsPCAP` + `SSLDumpRead/Write` (already set by `lab/play.sh`), Dolphin
writes one pcap per game boot under `<profile>/Dump/SSL/`, containing every socket of the emulated console:
- each TCP connection on its own ports, both directions, timestamps;
- **OPN's TLS traffic already decrypted** (the first bytes on port 8200 are a plain `ReqMemoryCheck` frame);
- UDP too: NAT negotiation (27901) and console-to-console P2P (kept for M4).

This beats the server logs: no interleaving between connections, both directions exact, and it works the same
whichever server is on the other end.

### Recording

Setup: the lab (`../mh3sp/lab`), Python MH3SP pinned at **`dev` @ `f53b06c`**, native AltWFC, hosts redirection on.

`tools/record-scenario.sh <scenario> [consoles]` (written; first full run with the recordings):
1. checks hosts redirection, AltWFC and the game image; restarts Python MH3SP on a fresh `mh3sp.db`;
2. boots each console (`devN` → lab profile + MAC) and waits until every Dolphin window is closed;
3. imports the one new capture per console with `corpus import` and creates `scenario.toml` if missing.

### Format

```
corpus/<scenario>/
├── scenario.toml        # description, steps played, consoles, oracle commit, date, milestone that must pass it
├── raw/<console>.pcap   # original captures (small; kept so imports can be redone)
├── tcp/<console>-<nn>-<opn|lmp|fmp>.jsonl   # one file per TCP connection, in connection order
└── udp/<console>.jsonl  # UDP datagrams (NAT negotiation, P2P), unused until M4
```

One JSON object per frame: `{"t": <µs since scenario start>, "dir": "c2s"|"s2c", "seq": n, "id": "60810100",
"name": "ReqMemoryCheck", "payload": "<hex>"}`. Multi-console scenarios share one clock (same Mac), so their
files can be merged by `t`.

Importer (`corpus import`):
- [x] Parse pcap (Ethernet link type), keep TCP/UDP payloads; key TCP connections by (client port, server port).
- [x] Reassemble each direction and split into frames with the `wire` codec (frames can span segments, and one
      segment can hold several frames).
- [x] Name connections by server port (8200 → `opn`, 8201 → `lmp`, 8202 → `fmp`); ignore port 80 (NAS) and 53.
- [x] `raw/<console>.pcap` is a **filtered** copy: only game TCP (8200–8203) and non-DNS UDP. Nintendo login
      (HTTP) and GameSpy profile traffic identify the console and stay in the lab.
- [x] Fail loudly on anything that doesn't frame cleanly (that would be a capture or codec bug).
- [x] Validated on the first lab session's captures (2 consoles, 32 connections, ~2100 frames, ~3200 datagrams,
      two server restarts included): every stream frames cleanly and re-encodes byte for byte, so Dolphin's
      decrypted TLS stream is complete.

### Volatile fields

Some bytes differ on every run: PAT tickets, online support codes, Capcom IDs, server time, random IDs.
- [x] `corpus diff <a> <b>`: compares two recordings of the same scenario frame by frame and prints the byte ranges
      that differ, per packet name.
- [x] `corpus/masks.toml`: one entry per packet name and volatile range, each with a **variable name**
      (`pat_ticket`, `support_code`, `capcom_id`, `server_time`, …). Ranges are expressed as PatData field ids or
      `lp2_string` positions where possible, raw byte offsets otherwise.
- [x] Record `s02` twice to discover them (`s04` twice at step 8); extend the masks whenever a diff turns out to be
      volatile. Seven masks cover the single-console corpus: PAT ticket, Capcom ID, support code, the two
      `AnsUserSearchInfoMine` ids, server time, one time byte of a content blob.

### Scenarios for M0

| Id | Consoles | Played steps | Must pass from | Status |
|---|---|---|---|---|
| `s00-returning-new-slot` | dev1 | returning console logs in twice, the second time into an empty Capcom ID slot | M1 | ✓ recorded |
| `s01-terms-decline` | dev3 (first time) | go online, decline the terms | M1 | ✓ recorded |
| `s02-login-gate` + `s02-login-gate-2` | dev1 | go online, slot 1, Valor1, gate 1, return to title (twice, for volatile fields) | M2 | ✓ recorded |
| `s02-first-login-gate` | dev4 (first time) | same, accepting the terms first | M2 | ✓ recorded |
| `s03-city-quest-solo` | dev1 | gate, create city, create quest, start, abandon, back in city, return to title | M2 | ✓ recorded |
| `s04-city-join` + `s04-city-join-2` | dev1 + dev2 | dev1 creates a city, dev2 joins it, dev2 leaves (twice; chat left for a later scenario with keyboard emulation) | M3 | ✓ recorded |
| `s05-quest-party` | dev1 + dev2 | dev1 city + quest, dev2 joins city and quest, start, abandon (includes a party host handover) | M3 | ✓ recorded |
| `s06-disconnect` | dev1 + dev2 | dev2 in dev1's city, dev2's Dolphin closed abruptly | M3 | ✓ recorded |

Recording findings:
- Terms acceptance and decline are decided on the console: the server sends the same packets either way. A
  console that already accepted the current version downloads the terms without showing them; on decline it
  still asks for LMP's address, then disconnects without opening LMP.
- A returning console sends its online support code in `AnsConnection`; a first-time console omits it and the
  server issues one.
- `s02` vs `s02-2` differ in 10 packet types: Nintendo login token, PAT ticket (issued, then echoed by the
  console), Capcom ID, server time, one byte of a time-driven binary, and 2 bytes of `NtcCheatCheck`
  (a counter or timer in the game's anti-cheat report).
- Every recording starts on a fresh Python database, so slot 1 is always empty and gets a new Capcom ID.

Friends, blacklist, event quests and the trading post get their own scenarios with the milestone that needs them.

## 5. Replay harness (`crates/testkit`)

- [x] **Console client** (`testkit/src/client.rs`): connects like the Wii (TLS ≤ 1.0 with `AES128-SHA`/`AES256-SHA`
      on OPN, plain TCP otherwise), sends and receives `wire` frames.
- [x] **Replay**: for each console of a scenario, open its connections in recorded order. Before sending a recorded
      `c2s` frame, wait (with a timeout) until the `s2c` frames recorded before it have arrived; across consoles,
      `c2s` frames go out in recorded `t` order, so "dev2 joins after dev1 created the city" holds.
- [x] **Variables**: when a masked range is first seen in an `s2c` frame, bind recorded value → live value
      (e.g. the ticket the server just issued); in later `c2s` frames, replace the recorded value with the live one
      before sending. Values must keep their length; anything else fails loudly.
- [x] **Comparison**: each received `s2c` frame against the expected one: same id, then same payload outside masked
      ranges. Declared divergences (`corpus/divergences.toml`, see Reference policy) are accepted;
      empty in M0, since nothing is implemented yet. Ignored: `ReqLineCheck`/`AnsLineCheck` (time-driven). Report: scenario, connection, packet name,
      first differing offset, side-by-side hexdump.
- [x] **Targets**: `corpus replay <scenario>... [--host <ip>] [--before <command>]` (standard ports on one host);
      `--before tools/oracle.sh` gives each scenario a fresh Python MH3SP, as when it was recorded. Tests will
      start the Rust server in-process on random ports (step 7).
- [x] **Outcome per scenario**: `scenario.toml`'s `milestone` decides whether a failure breaks CI: every scenario at
      or before `testkit::corpus::REACHED` must replay cleanly against the Rust server
      (`testkit/tests/server_skeleton.rs`, a fresh in-process server per scenario). `REACHED` is `M0`, so none
      yet; a smoke test proves the harness reaches the skeleton over TLS (the first expected frame times out).
- [x] Validation against Python MH3SP: `corpus replay corpus/s* --before tools/oracle.sh --nudge` replays every
      single-console scenario clean (637/637 server frames, twice in a row): exit criterion 4 for `s00`–`s03`.
      Manual in M0; it becomes the nightly tier 4 job in M1. Multi-console scenarios follow at step 8.
- [x] `--nudge` (Python only): Python MH3SP stalls when frames arrive together (see the global roadmap's list of
      Python issues); after 500 ms without an answer the harness sends a harmless `AnsLineCheck` to wake it.
      The Rust server is replayed without it.

### Step 4 result: first replay against Python MH3SP

`s01-terms-decline` replays clean (16/16, Wii-style TLS included). The other scenarios differ only where step 5
is expected to act:
- volatile values: `AnsServerTime`, `AnsTicket`, Capcom IDs (`AnsUserObject`, `AnsUserListData`), the support code
  (`AnsLoginInfo`), byte 788 of the time-driven `AnsBinaryData`;
- the recorded PAT ticket sent back to a server that never issued it: the session isn't found
  (`AnsUserSearchInfoMine` empty, then `NtcShut` on FMP);
- to confirm after substitution: `AnsFmpListData(2)` byte 870, most likely a world server's population.
Server state matters: replays without a fresh server showed slots already taken and populations growing.

### Step 8 result: multi-console replay

The replay harness now runs consoles concurrently:
- console frames go out in the scenario's recorded global order (turns), so causes precede effects across consoles;
- a server frame is awaited only once every console frame recorded before it has been sent, then for
  `RECV_TIMEOUT` plus the delay it had after its last cause in the recording;
- the server frames expected between two console frames may arrive in any order (town relays follow the players);
- an unrecorded notification is tolerated (and listed) unless one with the same id is expected: a relay racing
  with a player leaving may arrive or not;
- with `--nudge`, a connection waiting for its turn keeps nudging Python too (a frame it sent may be stuck);
- `--retries` re-runs a failing scenario against Python, whose interleaved writes make two-player scenarios flaky.

`corpus replay corpus/s* --before tools/oracle.sh --nudge --retries 3` replays all ten recordings clean against
Python MH3SP: 2299/2299 server frames (`s04-city-join` on its second attempt; `s05` with one tolerated relay).
No new mask was needed for two players: the Capcom ID substitution covers every new packet.

## Order of work

1. Workspace + CI skeleton (green on an empty test).
2. `wire`: framing codec + catalogue (enough to read captures).
3. Importer; record `s01`–`s03`; framing round-trip test on the corpus.
4. Console client + replay against Python MH3SP for `s01`–`s03`.
5. Second recordings, `corpus diff`, masks + variables until Python replays clean.
6. Items + PatData + their tests.
7. Server skeleton (config, Postgres, Valkey, listeners, TLS) + harness reporting against it.
8. Record `s04`–`s06`, replay them against Python (multi-console ordering).

## Not in M0

- Any reply from the Rust server (M1 starts with `ReqConnection` and the login funnel).
- Typed packet structs, migrations with tables, Valkey keys: each arrives with the first feature that needs it.
- Tier 3 scenario DSL, tier 4 nightly job, tier 6 Dolphin automation (M1).

## Risks and open points

- **Capture fidelity**: assumed that Dolphin writes the decrypted TLS stream once and complete in both directions;
  the framing round-trip test on the corpus is what proves it.
- **Timing**: the server sends `ReqConnection` ~1.5 s after accepting, and the game reacts to timeouts; the harness
  waits on frames, never on fixed sleeps.
- **Stable inputs**: the consoles' saves keep their support codes between recordings; wiping `mh3sp.db` gives new
  Capcom IDs each time, which the masks must absorb.
