# Our own Nintendo login (NAS)

**Status: agreed** (2026-10-11)

Goal: the server answers the Nintendo login itself, so an operator runs one binary and no AltWFC (Python 2, as
root). The login also tells the server which Wii is connecting, which is the identity `schema.md` S4 waits for:
bans that survive a deleted save. Design: [`architecture.md`](../../docs/architecture.md) (decision 13), [`schema.md`](../../docs/schema.md) (`devices`). The roadmap item: "Our own Nintendo login" in
[`00. global.md`](00.%20global.md).

## What the research found (2026-10-11)

Verified in the decomp, AltWFC's code and logs, and the corpus, unless marked *inferred*.

**Only two services remain outside OPN, LMP, FMP and NatNeg**, and we already answer one of them:
- **NAS**: `POST http://naswii.nintendowifi.net/ac`. MH3SP's patch turns `https` into `http` (`patcher/riivolution/
  MH3SP.xml:28-36`), so it is plain HTTP on port 80.
- **The availability probe** on UDP 27900: `natneg::serve_qr` already answers it. Unanswered, the game waits about
  4 s and goes on (`dwc_nasfunc.c:2390-2460`).
- **Unused by MH3**: `/pr` (profanity; compiled in, never reached: nothing sets its mode, zero requests in AltWFC's
  logs), GPCM/GPSP (no `gp*` symbol linked), the server browser, QR heartbeats, gamestats, storage. Every one of
  AltWFC's other services logged only "Now listening" across the full-stack runs of 2026-10-08/09.

**The `/ac` exchange** (`DWCi_Np_CPUCopyFast.c:428-779`, parser `dwc_nasfunc.c:175-316`):
- Request: headers `User-Agent: RVL SDK/1.0`, `HTTP_X_GAMECD: RMHP`; a form body, every value base64.
- Three actions, in order: `acctcreate` (only when NAND's `/shared2/DWC_AUTHDATA` holds no user id; the reply's
  `userid` is written there and shared by every WFC game on the Wii), `login`, then `svcloc` (with `svc` empty).
- Common fields: `sdkver`, `gamecd`, `makercd`, `unitcd`, `macadr`, `lang`, `devtime`, `csnum` (the serial),
  `cfc` (the Wii number), `region`; `login` and `svcloc` add `userid` (13 digits); `gsbrcd` and `ingamesn` are
  empty for MH3.
- Reply: status 200, `key=value` joined by `&`, every value base64. The game requires `returncd` (1–99 succeeds;
  ≥ 100 becomes error -(20000+code)), `userid` on `acctcreate` (missing: a new account at every boot), and a
  non-empty `servicetoken` on `svcloc`. It never reads `token`, `challenge`, `locator`, `svchost`, `statusdata`,
  `retry`; `datetime` only sets a clock offset.
- *Inferred*: the headers AltWFC sends (`Content-Length`, `Server`, `NODE`, HTTP/1.0, connection closed) are not
  needed; NHTTP's response parser isn't decompiled. Copying AltWFC's shape is the safe default.

**The service token reaches our server** (`PatInterface.cpp:1264`, `:6306`): the `svcloc` reply's `servicetoken`
comes back in OPN's `ReqAuthenticationToken` and again as item 10 of the first LMP's `ReqLoginInfo`, the very
request where LMP knows the console (`lmp.rs`, `Stage::FirstVisit`). Each session gets a fresh token. The corpus
shows it for every console. Today `opn.rs:172` and `lmp.rs:104` decode it and drop it. So a NAS login ties to the
console that follows exactly, with no IP or time matching.

**What the NAS sees of a Wii**: `userid` (ours, or one Nintendo or Wiimmfi issued years ago), `macadr`, `csnum`,
`cfc`. In the lab every Dolphin profile shares one `cfc` (cloned NANDs); `macadr` and `csnum` differ. On Dolphin all
of them are settings a player can change: the token proves "this NAS login", not "this physical Wii".

## Decisions

- **D1. A device is its (`macadr`, `csnum`) pair, stored as HMAC-SHA256 with the `secret`** (the user, 2026-10-11).
  `devices (id, public_id, mac_serial_hmac UNIQUE, created_at, last_seen_at)`; a console
  points to the last device it was seen on (`consoles.device_id NULL`). The pair is what a real Wii can't change;
  keying on the NAS `userid` (the old sketch) lets a fresh `userid` dodge a ban, and the shared `cfc` of cloned
  Dolphin NANDs would ban households. HMAC because only equality is ever needed, and a leaked database then leaks
  no hardware identifiers. The NAS `userid` and `cfc` are not stored: nothing reads them. Rejected: one row per `userid`; a row per identifier set matched on any of them.
- **D2. A token we didn't issue is refused at OPN**, with a refusal text, no setting (the user, 2026-10-11): every
  console then has a device, and device bans can't be dodged through another NAS. Rejected: accepting it without a
  device; a setting.
- **D5. A banned device is refused at OPN**, like every other refusal, with its text (the user, 2026-10-11);
  `access_rules` gets its `device_id` target now. Rejected: `returncd` 3914 at the NAS (a bare error 23914).
- **D6. The NAS base64 alphabet is `A–Z a–z 0–9 . -`, padded with `*`** (verified on the PAL `main.dol`,
  2026-10-11): the alphabet sits in DWC's data at `0x80630578`, and the decoder (`0x80507880`) maps `.` and `>` to
  62, `-` and `?` to 63, stops at `*`, and accepts neither `+`, `/` nor `=`. AltWFC encodes with `+` and `/`: its
  tokens were corrupted whenever their base64 held one, unnoticed since nothing checked them. Our encoder uses the
  game's alphabet. (The standard `+/` alphabet next to it, at a `CONNECT`/`Proxy-Authorization` string, is NHTTP's
  proxy authentication.)
- **D3. Tokens live in memory**: a map from token to device, bounded like the NatNeg table, each token used up at
  LMP and expiring after 5 minutes (the user, 2026-10-11). A restart disconnects every console, which then goes
  through the NAS again, so nothing is lost. Rejected: a table.
- **D4. The NAS serves plain HTTP on its own axum listener**, `nas_address` (default `0.0.0.0:80`), open in
  maintenance mode too: the console must reach OPN to read the maintenance text (the user, 2026-10-11). The game's
  URL is `https://`; MH3SP's patch makes it `http://`. HTTPS would protect the MAC, the serial and the token on the
  way, but needs the DWC TLS client to trust a certificate we sign, and whether it uses the CA the patch replaces
  is unknown: a question for the patch milestone, beside its per-deployment CA. Port 80 needs root or
  `CAP_NET_BIND_SERVICE` in the image. Rejected: `health.rs`'s hand-rolled HTTP; HTTPS now.
- **D7. The `userid` handed out on `acctcreate` is 13 random digits** (the user, 2026-10-11). Real Wiis bring user
  ids from Nintendo or Wiimmfi, so ours could collide either way, and nothing keys on it (D1). Rejected: a
  sequence.

## Tests

- The `/ac` codec: decoding a request, encoding a reply, the alphabet (D6).
- The three actions over HTTP against the real server, with a Dolphin-shaped form.
- The token: issued at `svcloc`, accepted at OPN, linking the console at LMP; an unknown or expired token refused
  (D2); a device row created and found again; a device ban refusing a console with a new save.
- The testkit's client logs in through the NAS before OPN, so every existing scenario uses it.
- [lab] A fresh Dolphin (no `DWC_AUTHDATA`) and an existing one, through our NAS alone, AltWFC stopped.

## Not in this sprint

- `/pr`, GPCM/GPSP, gamestats, conntest: MH3 doesn't use them.
- HTTPS for the NAS: the patch makes it HTTP.
- Rewriting the NAS hostname in the patch: the patch milestone.

## Done when

- A fresh Dolphin logs in and plays through our server alone; AltWFC is out of the lab, the README and the docs.
- The decisions are in `architecture.md` and `schema.md`; `recordings.md` has the run.
- `scripts/ci-tests.sh` and CI are green.
