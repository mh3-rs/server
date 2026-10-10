# Review: the state of the project and the next sprints

**Status: done (2026-10-11)**

A sprint about the next sprints. M0–M5 are built, two experiments are open (quest bots, end-to-end tests) and M6 is a
checklist written two days ago. Before going on, we take stock: what is really done, what is half done, what is
known to be left, what nobody has looked for yet, and what is only nice to have, across the project's repositories
(`core`, `brain`, `tunnel`, D11). No feature code.

**The target** (D2): an open-source server (AGPL-3.0) that anyone with some Docker knowledge runs from the README and a
guide; published on the Gitea registry, Docker Hub as the very last step. "Needed" means needed for that: a stranger
runs the server, and strangers play on it.

## Goal

Leave the review with **one ordered, sized list of what stands between today and that target**, a short list of what
we could do beyond it, and the next sprint named, so each later sprint starts from its line in the list instead of a
fresh investigation.

## Output

1. **`00. global.md` rewritten** (D4): a short "Done" (each line with its proof); "To do" in order, grouped into
   milestones, each item with its size (days), its owner (us, the user, the lab), its unknowns and enough detail to
   start; a few lines of "Could do", each with value and cost; why the order is what it is.
   **No numbering** (no `M6`, no `1.`): the order is the order of the lines, so reordering is moving lines, not
   renumbering every reference. Every item is a checkbox line (`- [ ]`), ticked in place (`- [x]`) when done, so the
   file shows its own progress; a finished group then shrinks into the "Done" section.
2. **`../docs`**, a repository with what lasts (D7), and `core/roadmap/` holding only active plans.
3. **The schema sketch** (D1) in `../docs`, with any change to an existing table it calls for listed as a to-do.
4. **`LICENSE`** (AGPL-3.0, D9).
5. **This file's Outcome**: what each discovery method found, what was dropped and why, the next sprint.

## Steps

In order. The **checkpoints** are discussions with the user: nothing after them is written as settled until they
are agreed. Findings collect in a "Findings" section of this file as they come, each tagged *to do*, *could do* or
*dropped*.

1. **Housekeeping** (first: the rest writes into the new places): `LICENSE`; the `../docs` repository; the lasting
   files moved there and the done ones as history (D7); `experiment-e2e.md` marked dropped; the code's references and
   the CLAUDE.md files pointed to the new places; branches (D8). Checked by `scripts/ci-tests.sh` and a grep of the
   references.
2. **Inventory** (sections 1–2): every "Done" claim checked and its proof named; the open items completed. Output:
   the draft "Done" and "Open".
3. **Discovery** (section 4): the methods of D5, the sweeps run by subagents in parallel (decomp and game data,
   coverage of the game, trust boundaries, failure paths, community), the walkthroughs (new player, operator) by
   hand because they need judgement. Output: findings, tagged.
4. **Checkpoint: the schema sketch** (D1). The sketch, and what in the existing tables it would reshape, discussed
   until agreed: the data model gets the most debate.
5. **Could do** (section 5): the brainstorm, fed by what discovery found; each idea with value and cost.
6. **Checkpoint: triage and order.** Every finding confirmed as to do, could do or dropped; the to-dos sized, grouped
   into milestones and ordered; the next sprint picked.
7. **Write**: `00. global.md` rewritten from the agreed triage; the Outcome; the user reads the result before it's
   committed.

## 1. Done: check the claims

`00. global.md`'s "Done" and "Against MH3SP" tables are checked against the code, the tests and the recordings,
not reread. Already found while drafting: the figures are stale (it says 86 tests and ~8,350 server lines; there
are 111 tests and ~19,100 lines of Rust across the crates). Each claim ends up as proven (a test, a recording, a
console run named), built but unproven on consoles, or wrong.

## 2. Open: what was started and not finished

The list so far, to complete during the sprint:

- **M5's console runs** (`recordings.md`): seats and Player Search on the Rust server, the hour-long session.
- **M4's mixed city**: a Dolphin on a second machine.
- **Quest bots** (could do, D3): sprint 2 (A5–A8) agreed, not started; then "the bot as a client of our own server"
  (to weigh); then sprint 3 (the companion). Known leftovers of sprint 1: the follow lags, the monster freezes while a
  bot owns it, the bot's HR shows 0, the quest icon's colour.
- **End-to-end tests** (dropped, D3): `feat/e2e-tests` holds three unmerged commits (test hooks behind a `test-hooks`
  feature, a DSU-input runner that gets as far as the quest counter).
- **Small unchecked items**: a refused jump (full or reserved city, M5); what a console does when its quest ends
  while the server is gone (M3 D4); voice chat through the relay's 20 kB/s cap; the 35.80.180.25 connection seen
  from Dolphin (architecture, open questions); the PAL-only sub-terms fields (`unk.md`).
- **Stale branches**: `experiment/e2e` deleted (step 1); `origin/experiment/bot` (squashed into `main` as sprint 1)
  to delete, by the user.

## 3. Known left: the M6 list, seen from Docker Hub

M6 as written in `00. global.md`: real-world networking, the US version, bans, the Nintendo login (NAS), our patch
and the player guide, DNS and hosting, coherency checks, backups, the release pipeline. Each item gets a size (days),
its unknowns and its owner (us, the user, the lab).

D2 adds items and changes some; the sprint details each as a to-do:
- **The license file**: AGPL-3.0, like MH3SP (D9). Added during this sprint (a file, no code).
- **The patch for the operator's domain**: already decided (M1 D3): the patch is generated for the deployment's own
  domain and downloaded, not hard-coded to ours; the to-do says how and from where (see the website idea, section 5),
  and how the certificate works for each operator.
- **The Nintendo login (NAS)**: before reaching Capcom's servers, every Wi-Fi Connection game calls Nintendo's
  authentication server over HTTP (`nas.nintendowifi.net/ac`: create an account, log in for a token, find the
  service's address). Nintendo shut it down in 2014; AltWFC (Python 2) and Wiimmfi reimplement it, and today we run
  AltWFC beside our server. "Our own NAS" means answering those few HTTP calls in our binary. Asking every operator to
  run AltWFC next to our image may be too much: whether it moves from "could" to "must".
- **The content**: `CONTENT_DIR` stays outside the image (the rule stands); the guide says where an operator gets it.
- **Publishing the image** (D10), a to-do of its own: multi-arch (arm64: a home server or a Raspberry Pi), safe
  defaults for someone who reads nothing, the configuration documented in one place; the Gitea registry first,
  Docker Hub as the very last step.
- **What is fit to publish**: the repository becomes public (a GitHub mirror later, not planned); the roadmap and the
  lab notes are checked for what shouldn't be.

## 4. Discover: what nobody has looked for

Methods, each producing items for sections 2, 3 or 5. Sweeps go to subagents, which bring back their conclusions:

- **The schema before 1.0.0** (D1): a guess at the most change-proof schema for everything M6–M9 and the "could do"
  might need, written down but not migrated.
- **The decomp and the game's data** (`/Volumes/USB/mh3/rmhp08.rvz`, the PAL disc): what the game can do online that we never triggered
  (menus, packets, features), and what its data makes possible (gear, quests, monsters) for "could do". Run during the
  sprint, not while planning it.
- **A new player, from nothing**: every step from "I have a disc and a Wii" to a city, written as the player guide
  would be, to find the steps we haven't built or decided (patch, NAS, DNS, certificate, Dolphin and real Wii).
- **An operator, from nothing**: deploy on a fresh host using only the README (image, compose, content, secret,
  domain, NAS); every missing step is an item. With D2, this is the main walkthrough.
- **Trust boundaries**: what a crafted or hostile client can do today: connection floods, oversized frames,
  requests out of order, a slow reader holding the world lock, relay abuse (amplification, foreign addresses).
  Against "strangers on the internet", not the lab.
- **Coverage of the game**: the client packets the game can send (the decomp's table, `PAT.MD`) against those we
  handle and those ever recorded; the menus never tried on consoles.
- **Failure paths**: a server restart, a full disk, a database error, the content folder missing or malformed:
  what players and the operator see.
- **The community**: what players of the existing MH3 servers (MH3SP's live server, Wiimmfi-era) ask for and
  complain about, to rank "could do" by what they want.
- Not run: load (D5), until strangers are about to come.

## 5. Could do: imagine

Not a list of what's written today, but a brainstorm: guess, extrapolate, imagine what a server in the middle of
every console's traffic, with the decomp and the game's data at hand, could give players and operators that the game
never had. Each idea gets its value and a rough cost; none is scheduled before section 3. Starting points, to widen:
the website (M7–M8), quest bots, anti-cheat (M9), joining a quest after departure, blocks keeping players apart,
city search, chat commands, the Japanese version, our own NAS, a public comparison page. Then further: what the
server sees (who hunts what, how long, with whom) turned into something players enjoy; what operators of a server
they didn't write need to run it for years; what the game's data and the bots' message map open up.

Already one idea (the user's, 2026-10-11): **the server is the core, an HTTP API; the website is only an SPA calling
it.** Everything stays in one place: one database and one migration source, an `openapi.json` the SPA (and anyone
else) builds on, no second backend. The website is one client of the API; a Discord bot, a CLI or anyone's tool
would be others. The patch for the deployment's domain becomes `GET /patch`, its address taken
from the core's configuration. To weigh in the sprint against M7's "admin API next to the health endpoints" (it
extends that rather than contradicting it) and the D1 schema sketch (accounts and the audit log then live in the
core's database).

### The brainstorm (step 5, 2026-10-11)

Value: who gains and how much. Cost: **S** days, **M** a week or two, **L** more, **R** research first. Not ranked
yet: the triage does that.

**For players: reasons to stay past HR 51** (the community's complaint: no endgame)
- **Leaderboards by quest**: the server sees a quest's departure and its end, so it knows who cleared what in how
  long. Fastest clears per quest and party size, on the website; weekly boards on the rotation's quest. High, M
  (needs the end of a quest read reliably, and a `quest_runs` table).
- **Server achievements**: first Alatreon clear, 50 Jhen Mohran repelled, every event quest done; shown on hunter
  pages. Medium, S once runs are stored.
- **Community events**: a Jhen Mohran weekend announced in game with a countdown, a "quest of the week", seasonal
  rotations planned on a calendar. High, S–M (the rotation and announcements exist).
- **Custom event quests**: an editor on the website (MH3SP's JSON format is known), community-made quest packs, a vote
  for next week's rotation. High, L (and a validation story: a broken quest hangs four consoles).
- **Quest bots** (sprint 2 agreed): a companion for a solo hunter; later, bots filling a party for Jhen Mohran, a
  training partner for new players. High, L.
- **Joining a quest after departure** (research). High if the game copes, R.

**For players: finding each other** (today: only through Discord)
- **The live world on the website**: who's online, which cities fill, which quests look for members, at a glance.
  High, M (the admin API, read-only).
- *dropped* **A Discord bot, a chat bridge** (2026-10-11): the website does all of that. The core is an API and the
  website one client of it; a Discord bot would be just another client, anyone's to write, nothing in the core.
- **Chat commands**: `/w` (a whisper: the game has none), `/who`, `/bot`, `/kick`, `/report`. Medium, S each.
- **System messages**: "a friend removed you", "welcome back, here's this week's event", over the inbox or the
  announcement path. Low–medium, S.
- **Hunter pages and friend finding**: a hunter's gear and HR, a Capcom ID to share. Medium, M.

**For players: comfort**
- **Other PAL languages** (French, German, Italian, Spanish content). Medium for those players, S–M.
- **The Japanese version**. Medium, L (different formats, the paid-era packets).
- **A word filter the console applies itself** (`Vulgarity` lists from `CONTENT_DIR`). Low, S.
- **Wii Speak through the relay** (voice works for direct pairs; check the relay's cap). Low, S (a lab run).
- **Bigger cities**: Japan's game had 10 per city; whether a Western disc copes with more than 4. Medium, R.
- **Keyboard chat on Dolphin** (the Wii version takes a USB keyboard). Low, R (a Dolphin question).

**For operators**
- **`mh3sp init`**: one command that writes the secret, the CA, the certificate, the patch and a compose file for a
  domain. High (the operator's walkthrough's biggest gaps), M.
- **Built-in backups**: a periodic job (`VACUUM INTO`), with retention. Medium, S (with the periodic jobs).
- **Periodic jobs and purges** (S11). Low now, S.
- **A server directory**: opt-in servers announce themselves to a public list; players pick one, the site serves its
  patch. Medium, M; it changes the project from "a server" to "a network of servers".
- **Import from MH3SP's database**: an operator switching keeps players' Capcom IDs and friends. Medium, M (MH3SP's
  MySQL schema is known).
- **Moderation**: `/report` with the last chat lines kept in memory as evidence, flags, bans with the game's own
  screens (`ReqWarning`, the ban screen), kicks out of a quest (`NtcCircleKick`). High once strangers come, M.
- **Dashboards and alerts**: a Grafana dashboard and Prometheus alerts shipped (NatNeg down was MH3SP's longest
  outage). Medium, S.
- **An update notice**: the server tells its admin a newer image exists. Low, S.

**Anti-cheat**
- **Profile and reward sanity from the disc's tables** (items, crafting, drops, read from the operator's disc into
  `CONTENT_DIR`). Medium, M.
- **`NtcCheatCheck`, memory checks, the game's own "modified data" ban warning**. Medium, R.
- **A hidden referee bot** that owns the monsters and sees every hit. High for integrity, L, after the bots.

**For the project**
- **A public "how it compares" page** (latency, the relay, the fixes). Medium, S.
- **A campaign for the original quest bytes**: ask the community for 2010–2013 captures and Dolphin dumps. Medium, S to
  ask, R to use.
- **RetroAchievements**: stay compatible, or provide what their set needs with our patch. Medium, R.

## Schema sketch (step 4, agreed 2026-10-11)

What the schema may need to hold after 1.0.0, guessed from everything above, to find what must change **now** in the
tables that exist. Not migrated: new tables come with their feature.

**The lens.** In SQLite, a forward migration that adds a table, a nullable column, an index, or renames, is one
statement. Changing a `CHECK`, a `NOT NULL`, a foreign key or a type means rebuilding the table (create the new one,
copy, drop, rename, recreate its indexes). So a change-proof schema gets its **constraints** right now; columns can come
later.

What may come, and from where:

```sql
-- People who sign in (website, admin CLI): players and staff alike, the role says which (S2).
users        (id, public_id, email UNIQUE, password_hash /* argon2 */, role REFERENCES roles, created_at)
consoles     + user_id NULL REFERENCES users ON DELETE SET NULL      -- pairing (M8): nullable column, cheap

-- API tokens, Bearer auth, issued by the binary or the website (the user's design, S3).
tokens       (id, key UNIQUE /* `<prefix>_<public>` */, owner NOT NULL REFERENCES users, usage REFERENCES usages,
              hash UNIQUE /* SHA-256 of `<prefix>_<public>.<secret>` */,
              created_at /* expiry = created_at + the usage's lifetime */,
              revoked_at NULL, revocation_reason NULL, CHECK ((revoked_at IS NULL) = (revocation_reason IS NULL)))

-- What our own Nintendo login sees of a Wii (S4): hardware identity, for bans that survive a deleted save.
devices      (id, nas_user_id UNIQUE, mac, serial, created_at, last_seen_at)
consoles     + device_id NULL REFERENCES devices                       -- if the server can link the two (to dig)

-- Bans and the whitelist (could do), one table (S5).
access_rules (id, public_id, kind REFERENCES rule_kinds /* ban | allow */,
              console_id NULL, hunter_id NULL, device_id NULL, user_id NULL, CHECK (exactly one target), reason, created_by REFERENCES users, created_at, expires_at NULL,
              revoked_at NULL, revocation_reason NULL)

-- Who did what (S7): append-only.
audit_log    (public_id PRIMARY KEY /* UUID v7 */, user_id NULL, token_id NULL, action, target /* a public id */,
              details /* JSON */, created_at)

-- Later, additive, nothing to reshape: anti-cheat flags (hunter, kind, evidence JSON, reviewed_by),
-- quest runs and their hunters (stats, hunter pages), profile history, settings editable at runtime (S9), texts.
```

Questions, the most consequential first:

- **S1. Enumerations that grow: a side table and a foreign key** (agreed 2026-10-11). Where an enum or a `TEXT` with a
  `CHECK` would be annoying to extend (token usages, roles, rule kinds), a lookup table (`usages (name PRIMARY KEY)`)
  and a foreign key: a new value is an `INSERT`, still enforced by the database. `CHECK` stays for the sets the game
  fixes. Rejected: plain `TEXT` guarded only by Rust; `CHECK` everywhere with rebuilds.
- **S2. One `users` table with a role**, for players and staff, instead of `accounts` plus `admins`: a moderator is a
  player too, and tokens and the audit log point at one table. The role is a growing set (S1). Recommend.
- **S3. Tokens** (format agreed 2026-10-11): a token reads `<prefix>_<public>.<secret>`. The database stores
  `<prefix>_<public>` (to find, list and revoke it; `<public>` is its own random identifier, no UUID) and the SHA-256
  of the whole token (it is high-entropy: argon2 is
  for human passwords). Open: how the first admin exists before any user (recommend `mh3sp user add --admin
  <email>`, printing a token once); whether pairing codes (M8, 10 minutes) are a token usage rather than memory.
- **S4. A console's identity: to discover** (2026-10-11), in the Nintendo login to-do. Today the support code's HMAC: a deleted save is a new console, so bans don't hold.
  With our own Nintendo login the server sees the Wii's NAS user id, MAC and serial, before OPN. Open, to dig in the
  NAS to-do: whether the server can tie a NAS login to the OPN connection that follows (a token the game carries, or
  at worst the address and time). The sketch keeps it additive (`devices`, a nullable `consoles.device_id`).
- **S5. One `access_rules` table** for bans and the whitelist, a target among console, hunter (Capcom ID), device, user,
  with a `CHECK` that exactly one is set; versus a table per target. One table, one query for `allowed()`.
  IP addresses are never stored (the logging rules); an IP ban, if ever, lives in memory. Recommend.
- **S6. Revoke, don't delete**: what is revocable (tokens, rules) carries the pair `revoked_at`, `revocation_reason`,
  set together; the audit log says who. A convention for every such table. Recommend.
- **S7. The audit log is append-only in the database**: triggers refusing `UPDATE` and `DELETE`. Its actor is a user, or
  NULL for the operator at the server's own CLI; the token used, when there is one.
- **S8. Changes to existing tables, now, before 1.0.0** (each a rebuild later):
  - `messages.from_hunter` nullable, for messages from the server or the website ("a friend removed you", M8):
    `NOT NULL` → `NULL` later is a rebuild. Or system messages go by chat only, and nothing changes.
  - `consoles.region` adds `RMHJ08` (the Japanese version is a could do; its check value costs nothing now).
  - The growing enumerations of S1, if any exist today: none (the existing ones are the game's).
  - The ranges of `hunter_profiles` and `hunter_equipment` that copy the game's validation as we inferred it
    (`title`, `item`, `unknown_*`…) leave the schema: `profile.rs` checks them, where widening one is a line, not a
    rebuild with players dropped meanwhile. The certain ones stay (slots, gender, lengths).
  - All agreed (2026-10-11).
- **S11. Nothing is deleted for now** (2026-10-11): consoles and hunters only grow. Purging (and the foreign keys a
  purge needs, e.g. bans that must outlive what they target) comes with a periodic job, a *could do*.
- **S12. No design for Discord in the schema** (2026-10-11): sign-in providers are the website's business, not the
  core's; `users.password_hash` stays `NOT NULL` (agreed earlier as nullable, then dropped with Discord). Accepted
  cost: if the website ever signs in through a provider via core users, that column becomes nullable by a rebuild.
- The rest as recommended (S2, S5, S6, S7, S9, S10), unchallenged.
- **S9. Settings: boot versus runtime.** What the server needs to start (ports, paths, secret, domain) is the
  deployment form (env or TOML, its own to-do); what an admin edits while it runs (access mode, texts, the rotation)
  is a `settings` table, when something edits it. Recommend that split.
- **S10. SQLite stays**: the core owns the only database, the website goes through the API; one process holds hundreds
  of players. A change of engine is the costliest change there is: worth saying it's decided.

## Triage (step 6, agreed 2026-10-11)

Every *to do* above, grouped into milestones in order (T1); sizes in working days, rough. Each point says how it can
be done:
- **[auto]**: by Claude alone, checked by tests and CI;
- **[lab]**: needs a console run (the user at the controls);
- **[user]**: needs the user's decision, input, an account or a push.

The order puts the unknowns early (the Nintendo login and the patch may change the deployment), the known work after.

**Cleanup and correctness** (~5 d): what exists, made right before building on it.
- [auto] Docs: the relay rule is 10 ms; "Against MH3SP" corrected (bans are a feature we miss; the untested claims).
- [user] Architecture decision 9 against the code (database errors drop the player): which one changes.
- [auto] Players listed in arrival order: check, fix, test. [lab] to confirm on consoles.
- [auto] The quests: the six without small monsters (MH3SP's inline fix), a check at load (missing files, shared ids,
  pouch overflow), the byte-identical test in CI (hashes, on the live branch's content).
- [auto] Unknown requests: identify each in the decomp, give it a safe default instead of a disconnect.
- [auto] The schema changes before 1.0.0 (S8).
- [auto] Tooling: `cargo deny` in the three repositories, `rust-toolchain.toml` in `brain` and `tunnel`,
  `gen_catalogue.py`'s paths, `fmp_progress` (gate or delete), the `#[allow]`s justified, the misplaced doc comment.
- [user] `TODOs.md` dissolved (it's yours, local).
- [auto] `recordings.md`: every console run still to do, in one list (T2).

**Our own Nintendo login** (~5–8 d):
- [auto] Research: what the game calls (`/ac`: account creation, login, service location; anything else), from the
  decomp, AltWFC and the captures.
- [auto] Discover whether a NAS login can be tied to the console's OPN connection (S4).
- [auto] The NAS in the binary, tested against recorded exchanges.
- [lab] A fresh Dolphin through our NAS alone; then AltWFC leaves the lab.

**The patch and the CA** (~8–10 d):
- [auto] Research: every hostname field in `main.dol` (PAL and US), which accept an IPv4 address (the DWC resolver,
  the NAS URL, the certificate's name).
- [auto] The generator for a deployment's domain: Riivolution XML (Wii, Dolphin) and an ISO patch (USB loaders),
  cross-platform.
- [auto] A CA per deployment embedded in the patch, a certificate from it; the TLS key out of the repository and the
  image, a script generating it.
- [user] The US disc image (only PAL is on the USB disk).
- [lab] Dolphin with the generated patch, SSL verification on and off; the US disc; a real Wii if one is available.

**Safe to leave running** (~8 d):
- [auto] The health port on 127.0.0.1 by default (T3).
- [auto] Connection limits (total, per IP); a write timeout; `panic = "abort"` and fewer `expect`s under the world
  lock; rate limits on chat, relayed packets and quest passwords; LMP only after OPN, and caps on console creation;
  the NatNeg table bounded; secrets never in trace logs. Each with a test (floods and slow readers in testkit).
- [user] The limits' values (players per IP, chat rate): defaults proposed, the user decides.

**The deployment form** (~5 d):
- [user] The configuration's shape: env, TOML or both (a discussion, with a proposal).
- [auto] Its implementation, documented in one place; a compose file with everything (server, content, secret,
  certificates); the README as an operator's guide (TLS, secret, content, hostnames, ports, maintenance mode, backups
  with the secret, upgrades, sizing, host networking's limits, an example alert on NatNeg).
- [auto] The image pipeline (Gitea registry, multi-arch) and the release pipeline, checked with `woodpecker-cli exec`.
- [user] Registry and pipeline secrets; pushes.

**The player guide** (~3 d):
- [auto] The draft: Wii (softmod, Riivolution or a USB loader) and Dolphin (normal speed, the SSL option, MAC), the
  patch, the support code as a credential, an error table.
- [lab] Followed once from scratch on a fresh Dolphin.
- [user] Whether and where it's published.

**Real-world validation** (spread over the milestones, then a final pass):
- [lab] Every run in `recordings.md`: seats, Player Search, a refused jump, stop warnings in a hunt, a quest ending
  while the server is gone (M3 D4), the mixed city on two machines, consoles behind home routers, a real Wii (legacy
  TLS), the US disc, a smoke run per quest (~2–3 h), the hour-long session.
- [user] A second machine, a real Wii, a home router: what's available.

**1.0.0** (~2 d):
- [user] The decision (only the user decides 1.0.0) and the Docker Hub account.
- [auto] The release, the image on Docker Hub as the very last step. From then on migrations only go forward.

Total: roughly 45–50 working days, most of them [auto], plus the lab runs.

**Could do**, in no order yet (one line each in `00. global.md`): the core as an API with the website as a client
(admin API, tokens, users, the live world, hunter pages, calendar, editing); bans and the whitelist (with the game's
refusal screens, `NtcShut`, `AnsLoginInfo` status 5, `ReqWarning`); leaderboards and achievements (quest runs);
community events; custom event quests; quest bots; joining after departure; chat commands and whispers; system
messages; other PAL languages; the Japanese version; periodic jobs (purges, backups); `mh3sp init`; import from
MH3SP; a server directory; moderation tooling; dashboards; anti-cheat; more quests online and the original bytes;
bigger cities; Wii Speak through the relay; a comparison page; RetroAchievements.

**Dropped**: the end-to-end runner; a Discord bot and chat bridge (API clients, anyone's).

Settled:
- **T1. The order as above** (2026-10-11): cleanup, the Nintendo login, the patch and the CA, safe to leave running,
  the deployment form, the player guide, validation throughout, 1.0.0. The next sprint is the cleanup. Rejected:
  hardening first; the deployment form first.
- **T2. `recordings.md` is one file again, in `../docs`** (2026-10-11), "Pending" on top: the list of what we have and
  what we should do, out of this repository. Rejected: the split.
- **T3. The admin endpoints on 127.0.0.1 by default now** (2026-10-11), tokens when the API grows (could do).
  Rejected: tokens now.
- **T4. Each to-do carries how it can be done** (2026-10-11): [auto], [lab], [user], in `00. global.md` too.

## Findings

Tagged *to do*, *could do* or *dropped* (provisional until the triage checkpoint); *fact* for what only corrects the
record.

### Inventory (step 2, 2026-10-11)

What "Done" rests on (`cargo test --workspace`: 110 passed, 1 ignored; the 19 scenarios replay):
- *fact* Proven by a test, a replay or a console run: M0–M4 entire, and in M5 party chat, announcements (in hunts
  too), stop, metrics, the position interval, jumps. The "Against MH3SP" claims hold, with the caveats below.
- *fact* Built but unproven on consoles: seats and Player Search on the Rust server (pending in `recordings.md`);
  whether a stop's warnings show in a hunt since party chat carries announcements (run 15 said no, before that).
- *to do* Players listed in arrival order: never tested, and the user is fairly sure it's wrong. Check and fix.
- *fact* Never asserted by a test: "profile ids stable" (they are the search ids, a
  SHA-256 of the Capcom ID: deterministic by construction); anonymous logs (a grep agrees); no stalled reads.
- *fact* Built and missing from "Done": the bots' P2P stack (~2,300 lines, in the default test run), packet
  encryption, malformed packets dropping the connection, discs without our patch refused.
- *to do* The byte-identical quests test passes vacuously in CI (`REFERENCE_DIR` unset): the claim rests on one
  manual run. Make CI run it, or say it's a manual check.

- *fact* `brain` (no behaviour yet, part B) and `tunnel`: CI with fmt, clippy `-D warnings` and tests, pinned image,
  committed lockfile; no `cargo deny`, no `rust-toolchain.toml`.
- *to do* The relay latency rule is 10 ms (the user, 2026-10-11): the goal in `architecture.md` says 2 ms and M4
  had "forwarding p99 < 1 ms"; the docs say 10 ms.

Notes from the user (2026-10-11):
- *to do* **Settle the deployment form**: the configuration is environment variables, and the keys keep growing
  (ports, TLS, encryption, NAT, bots…): a TOML file, env, or both (which wins), documented in one place. A big one.
- *to do* **The TLS key isn't committed**: `certs/dev/server.key` leaves the repository and the image; a script
  generates it (dev, and an operator's first run). The README documents `TLS_CERT`/`TLS_KEY`, `ENCRYPTION` and the
  ports.
- *could do* **Bans and a whitelist**: a feature we fully miss (MH3SP: a hard-coded ban list, empty on the live
  branch, and an event whitelist). Moved from M6's must-do to could do.
- *to do* **Console runs, one list**: every run still to do, collected from the findings, in `recordings.md`, which
  stays the list of what we have and what we should do, kept up to date. (Step 1 split it: the done sessions went to
  `../docs/recordings.md`. To settle at the triage: one file again, or the split.)
- *to do* **The quests: do we have them all, and do they work?** For step 3: which event quests existed (Capcom's
  distribution, 2009–2013), which MH3SP's content has, where the missing ones could be found (dumps, the community,
  the disc), and how to know a quest works (byte-identical to MH3SP's generator says it matches MH3SP, not that the
  game plays it).

Open in `core`, not listed before:
- *to do* No dependency audit (`cargo deny`) in `core`, `brain` or `tunnel` (a global CI rule).
- *to do* No image pipeline: CI only checks; nothing builds or publishes an image (D10's to-do). Single-arch.
- *to do* `tools/gen_catalogue.py`'s default paths predate the move to `~/git/perso/mh3sp/` (works with explicit
  paths).
- *to do* `#[ignore] fmp_progress` (`testkit/tests/server.rs`), an M2 leftover: make it a gate or delete it.
- *to do* Three `#[allow]` without a justifying comment (`wire/src/packets.rs` ×2, `server/src/net.rs`), and a doc
  comment in `net.rs` attached to the wrong item.
- *to do* 33 client request ids are untyped (circle kick, host handover, break, `ReqTicket2`, `ReqLayerTell`, …):
  for the coverage sweep (step 3), against the decomp.
- *to do* The local `TODOs.md` (gitignored): its console runs duplicate `recordings.md`; one idea not in the roadmap,
  system messages to a player ("a friend removed you", over the announcement path): *could do*. Then delete it.

`brain` and `tunnel`:
- *to do* (with the bots) `brain`'s `follow` doesn't detach a bot from its previous hunter: it can follow two.
  Its README doesn't document the stdin commands. No test between `brain` and the server.

### Discovery (step 3, 2026-10-11)

Seven sweeps, read-only: the disc (`rmhp08.rvz`, read with `nodtool`), the decomp's coverage, trust boundaries and
failure paths, the community (web; Reddit and MH3SP's Discord out of reach: a sample), the quests, and two
walkthroughs by agents that knew nothing of the project (an operator with the README only, a player).

**Reaching the server** (the patch, the certificate, the Nintendo login):
- *to do* **The patch for a domain doesn't exist.** MH3SP's patch rewrites no hostname: it swaps the game's pinned CA
  and turns NAS and GameSpy's https into http; every player still needs DNS (Wii) or a hosts file (Dolphin). M1 D3's
  generated patch has to rewrite, in `main.dol`: the OPN host `mmh-t1-opn04.mmh-service.capcom.co.jp` (opn03 on the
  US disc; never in our list), `naswii.nintendowifi.net/ac` and `/pr`, `%s.available.gs.nintendowifi.net`,
  `gamestats.gs…`, and `natneg1–3.gs.nintendowifi.net`, packed 27 characters apart: **a NatNeg hostname fits in 27
  characters**, the tightest limit. For Riivolution (Wii, Dolphin) and an ISO patcher (USB loaders), cross-platform.
- *to do* **A CA per deployment.** The CA the patch installs is MH3SP's, and its private key is public: anyone can
  impersonate any server using it, and every operator shares `certs/dev`'s key. The generated patch should embed the
  deployment's own CA, and the server serve a certificate from it (the CN can stay `mh.capcom.co.jp`; the game seems
  not to check the name: to verify). This is the user's "a script generates the key", one level up.
- *to do* Dolphin with SSL verification on (its default) has never been tried with a patched CA; the lab turns it off.
- *to do* **The Nintendo login**: AltWFC (Python 2, as root) beside the server, undocumented (link, ports, hostnames,
  how to turn its NatNeg and QR off). The community calls today's self-hosting "too complicated". Our own NAS (a few
  HTTP calls) looks like a must for "anyone runs it".
- *fact* A fresh NAND or save needs nothing: AltWFC creates the account, the server issues a support code. An existing
  save from another server is adopted silently as a new console (new Capcom IDs, no friends).

**Running it** (the operator's walkthrough):
- *to do* **No published image, no public repository URL, no compose file, no `.env` example.** The Dockerfile mentions
  a private Gitea secret (for `tunnel`, only with `bot`: say so). The `docker run` example mounts no content: the
  server starts and serves nothing.
- *to do* **The README misses**: TLS (none at all), the secret (how to make it, that losing it loses every console),
  `CONTENT_DIR` (where, which branch, the mount), the NAS, every hostname to point at the host, maintenance mode
  (exists, undocumented), backups (with the secret), upgrades and migrations, sizing (relay bandwidth), that Docker
  Desktop and rootless can't do host networking. It points to `CLAUDE.md` and to the `docs` repository: not operator
  documentation.
- *to do* A failing NatNeg was MH3SP's longest outage (8 hours, nobody could join a city): alerting on it (metrics
  exist; an example alert).

**Safe to leave running** (trust boundaries and failures):
- *to do, high* **The health port** binds 0.0.0.0 without authentication: `POST /stop`, `/announce`, `/bot/*`. With
  host networking, a forgotten firewall rule exposes them. Bind 127.0.0.1 by default, or a token.
- *to do, high* **No connection limits** (total, per IP); TLS handshakes cost RSA-1024 each, uncapped. MH3SP's login
  server once hung on stray connections (fixed with a timeout; we have a 10 s one).
- *to do, high* **A panic under the world lock** poisons it: FMP dead, `/health/live` still 200, nothing restarts it.
  There are `expect`s under the lock. `panic = "abort"` in release (the container restarts), and fewer `expect`s.
- *to do, medium* **No write timeout**: a console that stops reading freezes its own task and stays as a ghost; each
  send to it logs another `warn`.
- *to do, medium* **No rate limits** on chat and relayed packets (a flood fills the others' mailboxes, which kicks
  *them*); argon2 on every quest password attempt.
- *to do, medium* **LMP reachable without OPN** (the disc, patch and access checks run only at OPN), and **console rows
  unbounded**: any client can create consoles and hunters in a loop.
- *to do, medium* The NatNeg table is unbounded for 60 s and scanned per packet.
- *to do, medium* **Database errors drop the player**, where architecture decision 9 says they fail cleanly with a
  message (code or doc to change).
- *to do, low* At `RUST_LOG=trace`, payloads are hex-dumped, support codes and tickets included (the rule: never
  logged, at any level).
- *fact* Fine as is: frames capped at 64 KiB, bounds-checked parsing, single-use tickets, scoping to one's own city
  and party, no amplification in QR or NatNeg, the relay only between negotiated pairs, the bot port on localhost,
  `SECRET` required, monotonic clocks.
- *could do* A login deadline (a client answering line checks stays forever without logging in); relay port
  exhaustion and `INIT` injection (need many IPs or a 32-bit cookie); the brain dying leaves idle bots.

**What the game can do that we don't** (the decomp, the disc):
- *to do* **French, German, Italian and Spanish PAL consoles probably can't download content**: the game asks for files
  `language × 15 + n`, we answer only English's 0x01–0x0f (and drop the rest). MH3SP serves all five (English text in
  each). A lab run with a non-English console.
- *to do* **Unknown requests disconnect the console.** Some have senders whose menus aren't known (quest search with
  filters, `LayerInfoSet`, circle kick): a harmless answer instead (M5 D15's rule).
- *to do* **Refusals with a text, mid-session and at login**, for bans and shutdowns: `NtcShut` (untyped; the only way
  to disconnect a player with a message), `AnsLoginInfo` status 5 with our text (one field; we always send 1),
  `ReqWarning` (server-sent: a text the player must read, the A button blocked N seconds; continue or refuse), and
  the game's own account-ban, warning and wait-queue screens.
- *fact* Dead in the binary (no sender): circle break, host handover, delete kick list, user status, the Agreement and
  Japanese packets. `ReqTicket2`/`ReqWarning` are sent by the server. `ReqLayerTell`, city search: as M5 found.
- *fact* The game's 30 network error texts (`net_err_msg`): #16 "Quest data does not match" (everyone in a city
  must get the same event files).

**The quests** (the user's questions):
- *fact* **All of them by name**: the 23 Western events (2010–2011 calendar) and the 12 arena quests are in
  `content/`; Japan's are the same quests renamed; Jhen Mohran is on the disc. MH3SP adds three of its own and one
  arena quest on no official list. Not confirmed: Japan-only events after March 2010.
- *fact* **None are Capcom's bytes**: MH3SP rebuilt each from videos and archived pages. Saves don't keep event
  quests; originals would only exist in captures or Dolphin dumps from 2010–2013.
- *to do* **Six quests spawn no small monsters**: they name `.dat` files that don't exist; MH3SP's fix (inline small
  monsters) is in its `event_whitelist/` and `dev`, not in what we serve.
- *to do* A check at load: missing files, a `quest_id` shared within a rotation slot (four ids are shared today, none
  in the same slot), arena pouch overflows.
- *to do* The byte-identical test in CI: MH3SP's generator run in CI (Python 2), or only the SHA-256 of the expected
  binaries committed (hashes aren't content); on the live branch's content.
- *to do* A console smoke run per quest (accept, depart, spawns, abandon): ~2–3 hours for all 40; full clears much
  longer.

**The community** (a sample):
- *fact* ~16,000 members on MH3SP's Discord; ~50 players at once (2023, the last figure). Players find each other
  and the setup guide only on Discord.
- *fact* The ISO patcher is a Windows `.bat`; players spend hours and give up. Patch files are a Google Drive link.
  The launch video's links were removed "for takedown reasons": a legal risk for any guide linking patched files.
- *fact* RetroAchievements' online set depends on MH3SP's patched ISO hash: a different patch may break it.
- *fact* Bans and cheat detection: asked since 2021, never built. Players leave at HR 51 (no endgame). Event quests
  and Jhen Mohran in groups are what they value; the 4-player city cap and chat spam are complaints.

**The user's answers on the discovery** (2026-10-11):
- *to do* **The patch rewrites domains, not DNS**: no DNS redirection for players. Where the operator's domain doesn't
  fit a string (27 characters for NatNeg), an IPv4 address does (15 at most). To dig: whether every field accepts an
  address literal (the DWC resolver, the NAS URL, the certificate's name).
- *fact* Dolphin accepts untrusted certificates with an option on: the guide says so. The CA per deployment still
  matters for real Wiis.
- *to do* **Replacing AltWFC is a must**: our own Nintendo login (NAS) in the binary.
- *to do* **API tokens**, website or not: the binary issues tokens (Bearer auth) stored in the database: a `tokens`
  table with a `usage` enum that scopes it, an `owner` (the user owning it), `created_at` (the expiry derives from it
  and the usage), and a nullable pair `revoked_at`, `revocation_reason`. The admin endpoints (health port's `stop`,
  `announce`, …) take one; if needed, a small admin CLI using a token. Goes into the schema sketch (step 4).
- *could do* **Languages other than English** (the French, German, Italian, Spanish content ranges). English only for
  now.
- *to do* **Identify the unknown requests**, and give each a safe default (a harmless answer, not a disconnect).
- *to do* **Look for more quests online**: Japan-only events after March 2010, and any original bytes (captures,
  Dolphin dumps).
- *to do* **The player guide is a must** (written; whether and where it's published, decided later, given the
  takedown risk).

- *could do* **Periodic jobs**: every N, the server runs maintenance (purging abandoned consoles and hunters, old
  rows), the last run kept in the database so a restart doesn't push the next one far away (S11).
- *to do* **The schema changes before 1.0.0** (S8): `messages.from_hunter` nullable, `consoles.region` with
  `RMHJ08`, the inferred ranges out of the profile tables.

Ideas raised, for step 5: a server-driven kick out of a quest (`NtcCircleKick`); ticket rotation (`ReqTicket2`); a
word filter served to the console (`Vulgarity`); "update your patch" (`AnsMediaVersionInfo`); `NtcReconnect` to send
a console to another server; validation of profiles and rewards from the disc's own tables (drops, carves, crafting,
shop, read from the player's disc into `CONTENT_DIR`); the disc's 162 quests as test cases; the game's own ban
warning and memory check for anti-cheat; endgame content; keyboard chat on Dolphin.

## Decisions

- **D1. The schema: guess the most change-proof design now, add tables when needed** (2026-10-11). From 1.0.0 the
  schema only moves forward, so the sprint sketches the tables M6–M9 and the "could do" may need (bans, accounts
  linking consoles, the audit log, flags with evidence, messages from the website, stats), and checks the existing
  tables (`consoles`, `hunters`, `friends`, `messages`) against that sketch. An existing table is reshaped now only if
  the sketch shows it would otherwise have to change after 1.0.0; new tables wait for their milestone. The sketch is
  kept with the schema's notes. Rejected: creating every future table now (YAGNI), and no review until just before
  1.0.0 (when the release is pressing).
- **D2. Open source on Docker Hub, run by anyone** (2026-10-11). The server is for operators as much as players:
  someone with some Docker knowledge runs it from the README and a guide. Section 3 lists what follows. Rejected: our
  own public server only (the operator's view would be an afterthought), a replacement for MH3SP's live server
  (importing its database, outside our hands).
- **D3. Quest bots are "could do"; the end-to-end tests are dropped** (2026-10-11). Bots are fun, nearly doable and
  of real value, but not needed: sprint 2 stays agreed and written for when we come back to it. The end-to-end runner
  is hard and not that useful: console runs stay by hand. Rejected: bots before M6; resuming the runner as a release
  check.
- **D4. `roadmap/` keeps only what is active** (2026-10-11). The done files leave `roadmap/` but are kept (what we
  learned is in them: 41 references to their decisions, like "M5 D11", sit in the code). `00. global.md` is rewritten
  as a short "done", then the to-dos with enough details to start each one, then a few lines of "could do". Where
  the done files go: D7.
- **D5. All the discovery methods but load** (2026-10-11), plus the decomp and the game's data.
- **D6. This file is `roadmap/review.md`** (2026-10-11), kept as the record of why the order changed.
- **D7. What lasts moves to a docs repository, `../docs`** (2026-10-11), shared by `core`, `brain` and `tunnel` (the
  global layout for a multi-repo project, each repository's CLAUDE.md importing its shared rules): `architecture.md`,
  the schema and its sketch, `unk.md`, the findings of `recordings.md`, and the done milestones and experiments as a
  history under their current names, so the code's references ("M5 D11") still resolve. `core/roadmap/` keeps only
  active plans: `00. global.md`, `experiment-bots.md`, the pending part of `recordings.md`, this file. The CLAUDE.md's
  "not yet on the global workflow" shrinks to the branching (`main` only, unchanged). Rejected: `roadmap/done/`
  (keeps lasting docs out of the shared place), deleting them (breaks the references).
- **D8. The end-to-end branch is pushed as-is and stays as the archive** (2026-10-11): `feat/e2e-tests` on the remote
  (pushed by the user), the local copy deleted; `experiment-e2e.md` goes to the history marked "dropped" with D3's
  reason. Rejected: deleting it (the test hooks may serve again).
- **D9. The license is AGPL-3.0** (2026-10-11), like MH3SP: a `LICENSE` file at the root, the manifests' `license`
  field, in `core`, `brain` and `tunnel`.
- **D11. `brain` and `tunnel` are part of the project, in every step** (2026-10-11): cleanup, licensing, the
  inventory, discovery and the to-dos cover them as they cover `core`.
- **D10. The Gitea registry first, Docker Hub last** (2026-10-11): publishing the image (multi-arch, safe defaults,
  documented configuration) is a to-do of its own, Docker Hub its very last step; a GitHub mirror later, unplanned.

## Tests

No code, so no tests. The checks are on the documents: every "Done" claim names its proof; every to-do has a size
and an owner; `00. global.md`, the milestone files and the code's references agree.

## Not in this sprint

- Any feature code, bots sprint 2 included.
- Load tests (D5), console runs (they go into the plan).
- Drafting the next milestone's file: picked here, drafted at its own `/sprint-start`.
- Moving to `dev`/`prod` branches.
- Publishing anywhere (D10): this sprint only plans it.

## Done when

1. Every "Done" claim in `00. global.md` names its proof (a test, a recording, a console run), or is corrected; the
   figures are current.
2. Every discovery method of D5 has run, and every finding is tagged to do, could do or dropped: none left unfiled.
3. The schema sketch and the triage are agreed with the user (the two checkpoints).
4. `00. global.md` is rewritten (D4): every to-do sized, owned, ordered and detailed enough to start; the next sprint
   named. The user has read it.
5. `../docs` exists as a repository with what lasts, `core/roadmap/` holds only active plans, the code's references
   resolve, and the CLAUDE.md files point to the new places (D7).
6. `LICENSE` (AGPL-3.0) is in; branches handled (D8; pushes and remote deletions by the user).
7. `scripts/ci-tests.sh` and CI are green (docs only: nothing should change them).

## Outcome

Written 2026-10-11; read and agreed by the user.

**What came out:** `00. global.md` rewritten from the triage (unnumbered, ticked in place, each to-do tagged
[auto], [lab] or [user]); the `docs` repository (architecture, schema with its later tables, `unk.md`,
`recordings.md` with both pending and done sessions, the history, the shared CLAUDE.md); AGPL-3.0 in `core`, `brain`
and `tunnel`; the next sprint: **cleanup and correctness**.

**What each method found** (details in "Findings"):
- *Inventory*: "Done" holds; two claims weaker than written (the quests test passes vacuously in CI; arrival order
  untested and likely wrong); no dependency audit, no image pipeline, a TLS key in the repository.
- *The disc*: every hostname the patch must rewrite and its room (27 characters for NatNeg); other PAL languages
  can't download content; the game's own refusal screens; named data tables for later ideas.
- *The decomp*: the 33 untyped requests sorted (dead, server-sent, possibly reachable); unknown requests disconnect;
  `NtcShut`, `AnsLoginInfo` status 5 and `ReqWarning` as refusals with a text.
- *Trust and failures*: the admin port open without authentication, no connection limits, a panic poisoning the world
  lock, ghosts on slow readers, no rate limits, LMP without OPN.
- *The community* (a sample): patching is the pain; bans asked for since 2021; players leave at HR 51.
- *The quests*: all of them by name, none Capcom's bytes; six spawn no small monsters.
- *The operator's walkthrough*: no image, no compose, no TLS, NAS or content instructions.
- *The player's walkthrough*: today's patch rewrites no hostname, and its CA's key is public.

**Moved by the review:** bans from must to could do; our own Nintendo login from could to must; the player guide to
must; other languages to could do.

**Dropped:** the end-to-end runner (hard, not that useful; kept on `feat/e2e-tests`); a Discord bot and chat bridge
(the website does it; other API clients are anyone's).

**Diverged from the plan:** the walkthroughs were done by agents knowing nothing of the project rather than by hand
(closer to a stranger); `recordings.md` was split in step 1, then made one file again in `../docs` (T2); the schema
got a nullable password for Discord, then lost it with Discord (S12).

**Left for the user:** the `docs` repository's remote and push; pushing `feat/e2e-tests` (then its local copy goes);
deleting `origin/experiment/bot`; pushing `brain`, `tunnel` and this branch when wanted.
