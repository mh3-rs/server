# M0 — Foundations

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
- [x] `scripts/ci-tests.sh`: what CI runs, natively or `--docker` in the CI image; CI calls this same script.
- [x] `.woodpecker/push.yml` (calls `scripts/ci-tests.sh`):
  - image `rust:<pinned>-bookworm` (has `libssl-dev`), `rustup component add rustfmt clippy`;
  - steps: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`;
  - services (added with the first test that needs them, step 7): `postgres:17` (user/password/db `mh3sp`) and `valkey/valkey:8`;
  - env: `DATABASE_URL=postgres://mh3sp:mh3sp@postgres:5432/mh3sp`, `VALKEY_URL=redis://valkey:6379`,
    `SQLX_OFFLINE=true`;
  - no build cache in M0; add one (agent volume) only if pipelines get slow.
- [ ] `cargo sqlx prepare --check` step as soon as the first `query!` exists (M1).

## 2. Server skeleton (`crates/server`)

Only what M1 builds on; nothing here gets thrown away.

- [ ] `mh3sp [config.toml]` (default `config.toml`). Config: listeners (`opn`, `lmp`, `fmp`: bind address +
      enabled), `external_address` (the address handed to consoles, never guessed from `0.0.0.0`), TLS cert/key,
      `database.url`, `valkey.url`. `DATABASE_URL` / `VALKEY_URL` env vars override the file.
- [ ] Startup: connect Postgres (pool) and run `sqlx::migrate!()`; connect Valkey (connection manager) and `PING`.
      Either unreachable → exit with a clear error.
- [ ] Listeners: one tokio task per listener, one task per connection. OPN is TLS through `tokio-openssl` with the
      legacy setup (decision 6 in `00. global.md`): `@SECLEVEL=0`, minimum protocol SSLv3/TLS 1.0, ciphers
      `AES128-SHA:AES256-SHA:DES-CBC-SHA:DES-CBC3-SHA:RC4-MD5:RC4-SHA` (same list as MH3SP `other/utils.py`).
- [ ] Logging per decision 8 of `00. global.md`: `tracing-subscriber` terminal output with `EnvFilter`, one span per
      connection (`conn`, `peer`, `role`).
- [ ] Each connection: read frames with the `wire` codec, log one line per frame (`RECV <name> id seq len`, hexdump at
      `debug`), keep the connection open until the peer closes it. No replies yet (M1 adds `ReqConnection`).
- [ ] Graceful shutdown on Ctrl-C (stop accepting, close connections).
- [ ] `certs/dev/`: copy of MH3SP `cert/server.crt` + `server.key` (+ `ca.der` for reference).

## 3. Packet layer (`crates/wire`)

Facts checked against MH3SP `dev` and MHTrIDA:

- **Frame header**, 8 bytes, big-endian: `size: u16` (payload length), `seq: u16`, `id: u32`. Payload ≤ 65535 bytes.
- **Packet IDs**: `0xCCNNKK00`: `CC` category (`0x60` Opn, `0x61` Rfp, `0x62` Lmp, `0x63` Fmp, `0x64` Layer,
  `0x65` Circle, `0x66` User, `0x69` Agreement), `NN` message, `KK` kind (`01` Req, `02` Ans, `10` Ntc). The low
  byte is always `00`: MH3SP and MHTrIDA list the **same 301 IDs**, none with a non-zero low byte.
- **Items** (tag byte + value): `1` Byte (`u8`), `2` Word (`u16`), `3` Long (`u32`), `4` LongLong (`u64`),
  `5` String (`u16` length + bytes), `6` Binary (`u16` length + bytes), `0` Custom (raw bytes, see quirk below).
  Tag `7` (Object) exists in MH3SP's enum but is never used.
- **Strings are bytes**, not UTF-8: names and texts can be Shift-JIS. Keep them as `Bytes` everywhere.
- **PatData**: `u8 count`, then `count × (u8 field_id, item)`. Field **order matters** for byte equality, so it is
  stored in order (`Vec`), never a map. Some answers only contain the fields the client asked for: the request
  carries a byte list (`u8 count` + field ids), see MH3SP `pack_fields` / `AnsFmpInfo`.
- Other primitives: `lp_string` (`u8` length), `lp2_string` (`u16` length), byte lists (`u8` count + bytes).
- **Quirk**: the game mis-parses `LayerData` field `0x17` and swallows the next field id (MH3SP `FallthroughBug`,
  `mh/pat_item.py`); MH3SP writes a dummy `Custom` field `0xff` to be clobbered. `wire` must be able to emit that
  (`Custom` item); the record itself comes in M2.

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
- [ ] `Item`, `PatData` (ordered fields, `encode`, `decode`, `select(&fields)`), `lp_string`, `lp2_string`,
      byte lists; a small cursor-style reader/writer over `Bytes`/`BytesMut` with precise errors (offset, expected).
- [ ] Typed packet structs: **not in M0**. Each one arrives with the handler that needs it (M1+), so its shape is
      checked against the corpus at the same time.

Tests (tier 1):
- [ ] Unit tests for every primitive, including truncated input and oversize length.
- [ ] `proptest`: `decode(encode(x)) == x` for `Item` and `PatData`.
- [ ] Corpus test: for every recorded connection and direction, decoding the stream into frames and re-encoding
      gives back the exact bytes.
- [ ] `PatData` round-trip on real payloads that are known to be PatData: `AnsConnection` (`ConnectionData`),
      `NtcCollectionLog` (`CollectionLog`), `ReqCircleCreate` (`CircleInfo`).

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

`tools/record-scenario.sh <scenario>`:
1. checks the lab is up (MH3SP, AltWFC, hosts), deletes MH3SP's `mh3sp.db` (fresh accounts each time), notes
   the start time;
2. the human plays the scenario (each console booted fresh, so each has exactly one new pcap);
3. copies the new pcaps into `corpus/<scenario>/raw/<console>.pcap` and runs `corpus import`.

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
- [ ] Parse pcap (Ethernet link type), keep TCP/UDP payloads; key TCP connections by (client port, server port).
- [ ] Reassemble each direction and split into frames with the `wire` codec (frames can span segments, and one
      segment can hold several frames).
- [ ] Name connections by server port (8200 → `opn`, 8201 → `lmp`, 8202 → `fmp`); ignore port 80 (NAS) and 53.
- [ ] Fail loudly on anything that doesn't frame cleanly (that would be a capture or codec bug).

### Volatile fields

Some bytes differ on every run: PAT tickets, online support codes, Capcom IDs, server time, random IDs.
- [ ] `corpus diff <a> <b>`: compares two recordings of the same scenario frame by frame and prints the byte ranges
      that differ, per packet name.
- [ ] `corpus/masks.toml`: one entry per packet name and volatile range, each with a **variable name**
      (`pat_ticket`, `support_code`, `capcom_id`, `server_time`, …). Ranges are expressed as PatData field ids or
      `lp2_string` positions where possible, raw byte offsets otherwise.
- [ ] Record `s02` and `s04` twice to discover them; extend the masks whenever a diff turns out to be volatile.

### Scenarios for M0

| Id | Consoles | Played steps | Twice | Must pass from |
|---|---|---|---|---|
| `s01-terms-decline` | 1 | go online, decline the terms | | M1 |
| `s02-login-gate` | 1 | go online, accept, pick Valor1, enter gate 1, return to title | ✓ | M1 (login part), M2 |
| `s03-city-quest-solo` | 1 | gate, create city, create quest, start, abandon, back in city, leave | | M2 |
| `s04-city-join` | 2 | dev1 creates a city, dev2 joins it, chat both ways, dev2 leaves | ✓ | M3 |
| `s05-quest-party` | 2 | dev1 city + quest, dev2 joins city and quest, start, abandon | | M3 |
| `s06-disconnect` | 2 | dev2 in dev1's city, dev2's Dolphin killed | | M3 |

Friends, blacklist, event quests and the trading post get their own scenarios with the milestone that needs them.

## 5. Replay harness (`crates/testkit`)

- [ ] **Console client**: connects like the game (TLS on OPN with the same legacy settings, plain TCP otherwise),
      sends and receives `wire` frames.
- [ ] **Replay**: for each console of a scenario, open its connections in recorded order. Before sending a recorded
      `c2s` frame, wait (with a timeout) until the `s2c` frames recorded before it have arrived; across consoles,
      `c2s` frames go out in recorded `t` order, so "dev2 joins after dev1 created the city" holds.
- [ ] **Variables**: when a masked range is first seen in an `s2c` frame, bind recorded value → live value
      (e.g. the ticket the server just issued); in later `c2s` frames, replace the recorded value with the live one
      before sending. Values must keep their length; anything else fails loudly.
- [ ] **Comparison**: each received `s2c` frame against the expected one: same id, then same payload outside masked
      ranges. Declared divergences (`corpus/divergences.toml`, see Reference policy) are accepted;
      empty in M0, since nothing is implemented yet. Ignored: `ReqLineCheck`/`AnsLineCheck` (time-driven). Report: scenario, connection, packet name,
      first differing offset, side-by-side hexdump.
- [ ] **Targets**: any address. `corpus replay <scenario> --opn host:port --lmp … --fmp …` for manual runs; tests
      start the Rust server in-process on random ports.
- [ ] **Outcome per scenario**: `scenario.toml`'s `milestone` decides whether a failure breaks CI (milestone reached)
      or is only reported (not yet). During M0 nothing is reached, so the Rust run reports without failing.
- [ ] Validation against Python MH3SP: replay every scenario against the pinned Python server started from the lab;
      zero unexplained diffs is exit criterion 4. This run is manual in M0; it becomes the nightly tier 4 job in M1.

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
