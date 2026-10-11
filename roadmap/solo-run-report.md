# Solo run report (2026-10-11)

The run of [`solo-run.md`](solo-run.md): 08:07–09:10, well before the 13:00 deadline: every [auto] item of the
scope was done, and the rest is yours. Branch `feat/solo-run` in `core`, `docs`, `brain` and
`tunnel`, from `main`; nothing merged, nothing pushed. Every `core` commit passed `scripts/ci-tests.sh`.

## Done

**Cleanup and correctness: every [auto] item.**
- The relay rule is 10 ms everywhere (the test had two limits); "Against MH3SP" back in the README, each claim with
  its proof.
- Tooling: `cargo deny` in the three repositories' CI (the tool downloaded pinned at 0.20.2; tunnel's pipeline run
  green with `woodpecker-cli exec`), `rust-toolchain.toml` in `brain` and `tunnel`, the tunnel pinned at its
  licensed commit `d54e731` in `core` and `brain` (only its license and docs changed), `gen_catalogue.py`'s paths,
  `fmp_progress` deleted, the `#[allow]`s justified, `net.rs`'s doc comment.
- The schema (S8) as `schema.md` says: `messages.from_hunter` nullable (never on a friend request: a `CHECK`),
  `RMHJ08`, the profile tables keep the fields' widths only (the game's rules stay in `profile.rs`). Tested.
- Arrival order: the code was right; now tested (a player leaving and coming back included).
- Decision 9: a database failure answers the request with an alert ("Not available right now") and keeps the
  player; a notice goes unanswered; mail that can't be read waits for the next position. Tested by renaming tables
  under the running server (and checked that the mail test fails without the fix).
- The 33 unknown requests, each looked up in the decomp (`docs/unk.md`): 23 get an alert ("Not available on this
  server"), which cancels only that request on the console; 4 get a typed answer (`LayerInfoSet`,
  `LayerUserInfoSet`, the quest search's head and foot); the 6 a console never sends still drop it. Never status
  `0xFF`: the console drops itself on that one.
- The quests: the check at load lists every problem (missing files, two quests of a day sharing an id, a missing
  small-monster file, a pouch over 8); byte-identity in CI through `reference.sha256` (each quest JSON's hash to
  the binary MH3SP's generator makes, for `dev` and the live branch), no longer vacuous; `ci-tests.sh` always
  exports `CONTENT_DIR` (content tests skipped locally before).
- The six quests without small monsters: **no code change needed**, the server serves inline monsters already; the
  content has to change (a question).
- `recordings.md`: every console run still to do, in "Pending".

**Safe to leave running: every [auto] item.** The health port on 127.0.0.1 (`HEALTH_ADDRESS`); connection limits
(2000, 16 per address); a poisoned world lock stops the server (any other panic ends only its connection); a 30 s
write timeout; rate limits per console (relayed
traffic, chat, quest joins); LMP's first visit only after OPN and 5 new consoles an hour per address; the NatNeg
table bounded per address; no packet payload in any log; every refusal counted in `/metrics` (`limited`). Loopback
is exempt from the per-address limits (the lab and the tests run every console on 127.0.0.1), behind a `Config`
field so one test checks their wiring from another address. Each with a test.

**The deployment form, what doesn't depend on the shape:** the README as an operator's guide (what you need, a
quick start, every setting in one table, the secret, TLS, hostnames, ports, limits, maintenance, backups, upgrades,
sizing, an alert on NatNeg); `deploy/compose.yaml` (validated with `docker compose config`).

## The review

At the end, an agent that hadn't written the code reviewed the branch as a critic. It found, and I fixed (commit
"Fixes from the review"): the new-console cap bypassed by a made-up support code; LMP losing the console's support
code after a database failure (the console asking again would have been handed a new one); a message without a
sender able to block an inbox; `panic = "abort"` (my first take) letting any client-triggered panic restart the
server; NatNeg's cap counting finished negotiations; no test of the limits' wiring (all tests come from the exempt
loopback); `deploy/.env.example` ignored by your global `.env*` pattern; the image's uid not fixed. Two of my own
tests didn't prove what they claimed when first written (a decision-9 test passing with the fix removed, a race in
another): caught by breaking the code on purpose, rewritten. The end-to-end test of the relay limit, left
then, came in the second part (`relayed_traffic_is_cut_to_its_rate`: a flood cut near the burst of 40, the game's
peak of 9 a second passing whole).

## Provisional, and questions

All in [`questions.md`](questions.md): the limits' values; decision 9 at LMP (an alert where LMP may wait); the
content for the six quests; the configuration's shape (proposal: environment only); the pipelines (migrate to
`dev`/`prod` first); the private tunnel, which makes the image unbuildable without a token.

## To check in the lab

New in `docs/recordings.md`'s "Pending": decision 9 at login and in a city; arrival order with three consoles; a
session with the limits on (with `hosts.block` pointed at the Mac's LAN address: on loopback the per-address limits
don't apply).

## Left of the scope

[user] items only: `TODOs.md`, the limits' values, the configuration's shape (then its implementation), registry
and pipeline secrets, pushes. The image and release pipelines wait on the branching question.

# Second part: the notes (10:28–)

You were right: there was time left. The scope came from `../notes.md`
([`solo-run2.md`](solo-run2.md)), on `feat/solo-run2`, branched from `feat/solo-run`, in `core` and `docs`, and in
the new repositories `shared`, `cli` and `website` (their first commits, on that branch; `main` is still unborn).
Nothing merged, nothing pushed; every commit's checks green (`scripts/ci-tests.sh` in `core`, `scripts/ci.sh`
elsewhere).

## Done

- **The configuration from `mh3sp.toml`** (your answer to the configuration's shape; decision 7 rewritten): every
  key optional, an unknown key an error (a typo isn't a silent default), `secret__file` beside `secret`. Gone:
  environment variables (but `RUST_LOG` and `MH3SP_CONFIG`) and `dotenvy`. A parse error gives its line number,
  never the line (it may hold the secret). `scripts/dev.sh` writes the dev file, keeping your old `.env`'s secret
  and content folder (your `.env` now only has sqlx's keys and `RUST_LOG`, which nothing reads any more:
  `export RUST_LOG` for another level). `deploy/` mounts `mh3sp.toml`.
- **Chat commands** (`game/commands.rs`): a line starting with `/` goes to the server, its answer to the sender
  alone. `/tell <player> <message>` for everyone (not delivered through a black list); for the `admins` of the
  configuration (Capcom IDs): `/announce`, `/stop`, `/kick`, `/ban <Capcom ID> <reason>`, `/unban`, `/bot add|remove`.
  Others get "Unknown command".
- **The API** (decision 12, `api.rs`, axum on 127.0.0.1:8281): `/api/me`, and for admins `players`, `announce`,
  `stop`, `kick`, `tell`, `ban`, `unban`: one function per command, shared with the chat. Tokens (`users.rs`,
  schema.md's `roles`, `users`, `usages`, `tokens`) in the `mh3sp_token` cookie or a Bearer header;
  `mh3sp user add <email> <role>`, `mh3sp token add|revoke` make and revoke them.
- **Bans** (`access.rs`, the schema's `access_rules` and `rule_kinds`): a ban targets the player's console (all its
  hunters), kicks them, and refuses the console at login (an alert) and at every hop (LMP's second visit, FMP).
- **`shared`**: the API's messages. **`cli`**: `mh3sp-admin`, every admin command over the API (refuses `http://` to
  another machine: the token would travel in clear). **`website`**: SvelteKit 3, TypeScript, Tailwind, the Node
  adapter: sign in with a token (kept in a `Secure`, `HttpOnly`, `SameSite=Lax` cookie), the online list, every
  admin command. Each repository has `scripts/ci.sh` (format, lint, types, tests, cargo-deny or `npm audit`).

Checked end to end by hand as well as by tests: the server with the CLI (refusals, a revoked token, a token file,
https through native-tls) and the built website against the server (sign-in, a bad token, commands, a cross-site
POST refused, a revoked token signing out).

## The review

An agent reviewed the second part as a critic and found 13 points; fixed: a banned console still getting in with a
ticket from before the ban; `/tell` ignoring black lists; a command not in UTF-8 falling back to public chat (a
private tell shown to everyone); `/ban` by name able to hit someone else (Capcom IDs only now); the ban's author not
recorded from the API; a database failure answered 404; config admins compared with case; plain-http tokens from the
CLI; missing tests (every admin route and chat command refused to a player). Each fix's test was proven by breaking
the code. Left, in the questions: bans evaded by a fresh save (a new console, until `devices`); schema columns not
used yet (`expires_at`, the `allow` kind, `session`, `password_hash`), which `schema.md` sketched; a config warning
logged before the logger starts.

Also: Woodpecker pipelines for `shared`, `cli` and `website` (run green with `woodpecker-cli exec`, but `cli`'s,
which needs `shared` pinned); a draft of the player guide (`docs/player-guide.md`: the patch today, DNS or hosts,
Dolphin's settings, the support code as a credential, a table of the game's error codes from the decomp and MHTrIDA;
never followed, two details unverified and marked). Later: sign-in by email and password (`POST /api/session`, a
30-day session token; five failures an email in 15 minutes, then 429; an unknown email costs the same argon2 check;
`mh3sp user password` sets one; sign-out revokes the session on the server), the bans in force (`GET /api/bans`, in
the CLI and the website, to know whom to unban), texts through the API capped at 300 characters (a long paste
would have filled every mailbox and dropped everyone). A second review of these found, fixed: parallel sign-ins slipping past the throttle (now
counted before the check; 20 at once, 5 checked: tested), unbounded argon2 work and cache keys (one check per CPU at
once, emails and passwords capped), a duplicate ban breaking the website's table (bans have ids; tested with a
console banned twice), the CLI's loopback check fooled by `http://localhost:1@evil.example` or `HTTP://` (the URL is
parsed now), `group_concat`'s order. The website's Playwright tests run against the real server
(`npm run test:e2e`: sign-in, the cookie's flags, commands, sign-out, a player refused; the cookie check proven by
breaking it), locally only: its CI doesn't build `core`'s binary.

Last: the quest bots through the API (`POST /api/bot/add`, `/api/bot/remove`, and `mh3sp-admin bot`), so every
command of the notes is in the chat and the API.

Your dev environment: the schema changed (users, tokens, bans), so `scripts/dev.sh --reset` wiped the dev database
(twice); `mh3sp.toml` holds your old secret and content folder. The consoles in your Dolphin saves get new Capcom IDs
on their next login (their support codes are new to the database).

## For you

In [`questions.md`](questions.md): the API and its data model (yours to review: `schema.md`'s "Built since" lists
where I departed from the sketch), `shared` as a path dependency (Woodpecker's builds of `core` fail until it's
pushed and pinned), chat commands' choices, bans, the whitelist (a design question), two new licenses
(`BSD-3-Clause` in `core` for axum's router; `CDLA-Permissive-2.0` in `cli` for ureq's root certificates). In the
lab (`recordings.md`): chat commands and bans on consoles (can the keyboard type `/`? a tell from someone not there?
the banned alert at LMP?).
