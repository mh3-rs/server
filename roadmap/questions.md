# Questions from the solo run

Open points for the user, from the solo run (`solo-run.md`). Each: what, why it matters, the provisional answer
taken (if any). Delete an entry once answered.

## Provisional calls

- **Decision 9 at login.** A database failure during the LMP login (support code, slots) now answers the request with
  an alert ("Not available right now") instead of dropping the connection. Alerts are proven on consoles at FMP only; the research
  in the decomp couldn't confirm how LMP's login state machine takes one (it might wait instead of showing the
  text). Provisional: keep the alert (decision 9's wording); a lab check is pending in `recordings.md`. If it hangs,
  LMP goes back to dropping and FMP keeps the alert.

- **The limits' values** ("Safe to leave running"; [user] in the roadmap): each a constant with its reason, picked
  by me, to confirm or change.
  - Write timeout: 30 s (`net.rs`, `WRITE_TIMEOUT`), as long as an unanswered line check.
  - TCP connections: 2000 in total, 16 per IP address (`net.rs`); loopback has no per-address cap (the lab and the
    tests run every console on one address).
  - Per console (`fmp.rs`, token buckets; the corpus peaks at 9 relayed messages in a second, 42 in ten, and 1 chat line a second): relayed
    game traffic 20 a second (burst 40), chat 1 a second (burst 5), quest joins 1 a second (burst 5; a join over it
    gets "Too fast: please wait a moment."). Over the limit, a relayed message or a chat line is dropped quietly.
  - New consoles: 5 an hour per IP address (`game::Admission`); LMP's first visit within 10 minutes of passing OPN
    from the same address. Risk: a carrier-grade NAT that changes a console's public address between two
    connections would be refused at LMP (the encryption's keys in transit already assume the same address).
  - NatNeg negotiations under way per IP address: 32 (`natneg.rs`, `MAX_PER_ADDRESS`); a party needs 3 per console,
    and a finished one no longer counts.
  - Every refusal by a limit counts in `/metrics`' `limited` (logged at `debug` only, so a flood can't flood the
    logs): the operator's way to see a limit too tight.

- **Chat commands** (`feat/solo-run2`, from the notes): the admins are Capcom IDs in the configuration (`admins`)
  until the website's users and roles; an admin command from anyone else answers "Unknown command" (not "not
  allowed": it doesn't say there's something to try). `/tell` names a player by Capcom ID or by a name only one
  player online has; it reaches a player on a quest as party chat from themselves (as announcements do), with the
  sender's name. `/ban` and `/whitelist` wait for the schema's `access_rules` (S5), whose `created_by` names a user:
  an admin in the game isn't one yet (a question below).

- **The API** (`feat/solo-run2`, from the notes; architecture decision 12): axum on 127.0.0.1:8281, tokens in a
  cookie or a Bearer header, the first ones from `mh3sp user add`. The schema's `roles`, `users`, `usages` and
  `tokens` as `schema.md` sketched them, with its departures listed there ("Built since"): **the data model is
  yours to review**. A new license allowed for axum's router (matchit): `BSD-3-Clause` (AGPL-compatible).
  The health port's `announce`, `stop` and `bot` stay until the CLI replaces `mh3sp announce` and the rest.
- ~~**`shared` is a path dependency**~~: settled (the user, 2026-10-11): `shared` and `tunnel` are published to the
  private Gitea registry on a version tag, and pinned exactly; only the image will be public (Docker Hub, later).

- **Bans** (`feat/solo-run2`): a ban targets the player's console (all its hunters), checked at LMP's login, answered
  with an alert ("This console is banned from this server", unverified on consoles, like decision 9 at LMP), the
  player kicked at once. No expiry from the commands yet (the column exists). `/whitelist` isn't built: who would
  be allowed, and how (a new console has no Capcom ID to name before it's refused; an invitation code? an
  `allow` rule per console the admin sees in a list of refused ones?), is a design question for you.

- **The website's sign-in**: email and password for users the binary made (no registration, no reset, no email
  sent: a design question for the website's milestone). Failed sign-ins throttled per email (5 in 15 minutes),
  not per address (the website calls from one): **anyone can keep a known email (an admin's) locked out**, five
  wrong passwords every 15 minutes; the way out is a CLI token. Per (email, the visitor's address, forwarded by the
  website) would end that: your call. A sign-out while the API is down deletes the cookie but can't revoke the
  token (valid until its 30 days).
- **Left from the second review**: a banned player can come back as a new console (a save without a support code
  yet) until `devices` ties a ban to the Wii; `access_rules.expires_at` and the `allow` kind exist (the sketch's) without
  a user yet: keep, or drop until needed (YAGNI)? (`session` and `password_hash` are used now: the website's
  sign-in.) (The
  `secret`/`secret__file` warning lost before the logger is gone: both set is now an error.)

- **The website's end-to-end tests need `core`'s binary**: separate repositories make that a cross-repository CI
  job (or an image of the server to run as a service, once the image pipeline exists). Until then, local only.

## Questions

- **Which content the six quests without small monsters should come from.** The code already serves inline small
  monsters, byte-identical to MH3SP's generator. The live branch (`content/`, 7e21e0a) names `.dat` files that don't
  exist in six quests (Flooded Forest Free-For-All, Gobuled Everything in Sight, Shut Up and Fish!, Speak of the
  Deviljho, Mercy Mission, The Phantom Uragaan; two are in today's rotation). MH3SP made them inline (`eceab47`),
  then its live branch reverted `event/` to the `.dat` names (`793658d`, which also changed other fields); its
  `event_whitelist/` and `dev` keep the inline monsters, with the older other fields. So it's a content choice,
  outside every repository: (a) `CONTENT_DIR` on `dev` (monsters, older quest fields, its own rotation); (b) the
  live branch with the six files' `small_monsters` taken from `event_whitelist/` (a small script could merge
  them into a content folder; not written); (c) leave it, the check at load warns. Recommended: (b).
- ~~The configuration's shape~~: answered (`../notes.md`): a TOML file, with `__file` forms for secrets. Built in
  `feat/solo-run2` (architecture decision 7). Provisional: the file is the only source (environment variables are
  gone, except `RUST_LOG` and `MH3SP_CONFIG`); flat keys, named as the old variables in lower case.
- **The image and release pipelines.** The global workflow's (`rc.yaml`, `release.yaml`, `scripts/release.sh`)
  assume `dev` and `prod` and git-cliff versions; this project is `main` only until migrated (`docs/CLAUDE.md`).
  Recommended: migrate to `dev`/`prod` first, then copy them (multi-arch through the buildx plugin, the Gitea
  registry); the image would then be versioned from its first build. The alternative, an image per push to `main`
  tagged `main` and the commit, would be thrown away at the migration. Not started: the branching is yours.
- **The tunnel is private, and the image can't be built without its token**, bots or not: Cargo fetches every git
  source of the lockfile (tried: `docker build .` fails on `mh3sp-tunnel`). Anyone outside can't build the server.
  Recommended: make `mh3sp-tunnel` public (AGPL like the rest), or vendor it into `core`. Your call (a repository's
  visibility).

