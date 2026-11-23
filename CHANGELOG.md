# Changelog

## [0.1.1] - 2026-11-23

### Fixes

- **world**: The world keeps each fact once, so no packet sequence from one console stops the server or leaves a state behind

### Documentation

- **claude**: A check is designed from every writer of the data it guards

## [0.1.0] - 2026-11-22

### Features

- Solo runs 1 and 2: correctness, limits, TOML config, chat commands, the API, bans
- **nas**: Our own Nintendo login; OPN refusals shown on consoles (**breaking**)
- **patch**: A CA per deployment and the players' patch for its domain (**breaking**)
- **texts**: The server's name and texts come from the configuration
- **guides**: The player guide on /patch, the example config as the reference
- **world**: Rookie and expert servers keep to their HR
- **errors**: Known errors and error codes, a page of their own at /errors
- **deploy**: Published ports instead of host networking; the login logs its refusals (**breaking**)
- **check**: `mh3sp check <domain>` says which port forward or DNS record is missing
- Releases from dev to prod, one reported version, the plans on Gitea (**breaking**)
- The public server, stripped to the game (**breaking**)

### Fixes

- **patch**: One Host header from the Nintendo login, so a reverse proxy can front it; amd64-only image (**breaking**)
- **tests**: A notice that is due is waited for, not looked for in a 200 ms window

### Documentation

- **claude**: Keep only what is specific to this repo, the rest is global
- Review of the project, the plan rewritten, lasting docs moved to the docs repository
- **patch page**: Where Dolphin's certificate switch is, and that it needs nothing
- **readme**: For players and the project; the operator's guide becomes Docker Hub's page
- CLAUDE.md, the migration file's checksum and root-owned pipeline files
- CLAUDE.md says how a scripted test waits for a frame

### Build and CI

- An absolute CONTENT_DIR for the cloned content
- Shared and tunnel from the Gitea registry; main's image to the Gitea registry
- Content from a clone at the corpus commit when ../reference has moved
- **image**: The server's version as tags too (X, X.Y, X.Y.Z)
- Public mirrors: main to GitHub (mh3-rs/server), the image to Docker Hub (mh3rs/server) (**breaking**)

### CLAUDE.md

- Sources and working rules
- The working rules settled so far
- The lab's content checkout; no attribution lines in commits

### Compose

- Bind-mount data/ instead of named volumes

### Corpus

- S00-returning-new-slot; record-scenario.sh takes console names
- S01-terms-decline
- S02-login-gate twice; record script retries MH3SP startup
- S02-first-login-gate
- S03-city-quest-solo; M0 step 3 done
- S04-city-join (two consoles)
- S04-city-join-2
- S05-quest-party (two consoles)
- S06-disconnect (two consoles)
- S11-party-chat (chat in a quest party, recorded against MH3SP)

### M0

- Drop licensing note (decided: no license)

### M1

- Name length check now applies to config overrides

### M4

- Hybrid decides per pair (unit test with three consoles)
- The recorded NatNeg exchanges replay against the server
- Deployment, the README and the Dockerfile's UDP ports

### M5

- The unknown fields named; profile notices for one player routed
- City seats and arm wrestling, one player each
- Player Search
- S12 recorded (Player Search); what its replay found
- Jumps (D10–D13), s15 recorded
- S13 recorded (seats); a refusal in place of an answer can be declared
- S14 recorded; LayerTell not built (D14)

### Roadmap

- Add test strategy tiers, CI and E2E tasks
- CI on Gitea + Woodpecker
- Dev environment, CI database, DB tasks
- Live state in owner node memory, Valkey for shared temporary data (fleet phase)
- Reference policy (game is the constraint, Python is not)
- M7 admin website, relay latency target (Python: ~50 ms median)
- Event content in Postgres from M2; fleet registry in Valkey; metrics pulled from nodes
- Draft M1 (login funnel); schema policy before/after 1.0.0
- M1 decisions D1-D7 settled, Valkey decisions open; JP as future step; whitelist registration flow
- Start with dependencies down, unhealthy until first ready
- Dependency outages, unhealthy only before first ready, error logs for both
- What players experience during a dependency outage
- Hunter profile block (256 B) in memory M2, persisted and mapped M3; hunter pages and friends on the website
- Profile block validated in stages, rebuilt from fields once mapped
- Never store the raw profile block, only parsed fields
- Profile rebuild must round-trip every corpus profile before the raw block is dropped
- SECRET__FILE (Docker secrets) takes precedence over SECRET
- Kicks still open; kicks from the website as a future step
- Pending friend requests confirmed in the lab
- Stored messages confirmed in the lab (run 6)
- M4 drafted (NAT negotiation and relay)
- Unknown fields report and fix proposal
- M4's NatNeg item as built
- M5 drafted (parity and polish)
- The release pipeline as a future step (1.0.0)
- Recordings.md, every console session to do; M5's packet decisions
- The scope of 1.0.0
- Texts in the database, domain the only address key
- The domain sent to consoles, the relay resolving it
- The relay asks one resolver, the next only if it fails
- The listeners, a management socket
- The whitelist flow, the texts storage left to the schema
- The reachability check, mh3sp check <domain>
- Quieter and fuller probes for mh3sp check, to decide

### Schema

- Migrations/0001_schema.sql, verified on Postgres 18; dev.sh --reset; rebuild on migration changes

### Settings

- 'access' (open / on_register / admin); fleet not planned, no leases; standalone deployment; Valkey question open

### config

- Ports from the environment; masks by pattern for the ports

### content

- Quest binaries (0x06-0x0f) from MH3SP's quest files

### corpus

- S07-chat (city chat, a tell)
- S08-friends (request kept in the inbox, accepted later, deleted); M3 D2 from the console
- S09-blacklist; no blank black list entry; masks to the end again
- S10-party (password, not ready at departure, cancel)

### gitignore

- TODOs.md (local)

### net

- 250 ms pause before the first message, bisected on Dolphin

### recordings

- Voice chat through the relay, against its rate cap (later)

### replay

- Notifications relayed from another console may arrive late

### roadmap

- Console checks done (no Capcom ID deletion, support code shown)
- In-memory structures (moka for expiring data, dashmap, struct in a mutex)
- Sessions owned by their connection, handed off by ticket
- M1 exit criterion 1 met on Dolphin (server list screen is FMP's)
- M2 draft (protocol, open decisions)
- M2 decisions settled (event content as MH3SP's JSON files, resources on demand)
- M2 content read on demand, versions derived from the bytes
- Content stays out of the code repository and the image
- M2 done (Dolphin creates a city and starts a quest on the Rust server)
- M3 draft (protocol from the corpus, open decisions)
- M3 D2 (pending friend requests stored), D5 settled (one world lock, noted in code), D7 (the city host), D9 (quest capacity and password)
- M3 D7 (quest kick and kick list), D9 settled (quest password as an HMAC, dropped at departure)
- The world by level; joining a started quest as a future step; M3 D9 with argon2
- Quest password placeholder; which world numbers the game fixes; join-after-departure rules
- M3 decisions (D1 recordings, D3 profile stored raw and parsed, D4 lab test first, D6 settled, D7 and D9 to test, D8 later)
- M3 D2 marked settled
- M3 D2, a friendship is mutual (one ordered row per pair)
- M3 progress (layers, parties, chat, friends, black list; every recorded scenario replays whole)
- Portrait of the project after M5
- The global roadmap becomes status, comparison and what's left; lasting decisions move to architecture.md
- The jumps seen working on consoles
- MH3SP's list order is by slot; packet encryption as a future experiment; a comment back in place
- Two experiments drafted, end-to-end tests on Dolphin and quest bots
- Encryption done, no branch any more
- End-to-end tests before quest bots
- Quest bots sprint 2 (A5-A8) drafted, its research kept for part B
- Quest bots sprint 2 (A5-A8) agreed
- The solo run's rules, agreed
- Our own Nintendo login, agreed
- The Nintendo login over HTTPS, researched, a could do
- What is known for the patch and the CA
- The patch applied in the browser, a could do
- The user's answers to the open points; Docker Hub and GitHub, a draft plan
- The open questions in the roadmap; questions.md gone
- The review and the solo runs, done, moved to docs/history; the limits confirmed
- Big lines of the public mirrors sprint (GitHub, Docker Hub)

### server

- OPN login, refusal path, maintenance mode
- LMP login, accounts in SQLite, session hand-off by ticket
- FMP lifecycle, players registry
- FMP server selection (content downloads, world server list, pick)
- The world and gates; content from CONTENT_DIR
- Cities (list, create, enter, leave)
- Quests (circles); every solo scenario replays whole
- Players in a city see each other (M3, first part)
- City chat and tells (s07-chat replays whole)
- Friends and black list in SQLite; tells filtered by blocks
- Player search and cards; replay: early answers, declared omissions
- Quest parties (join, members, leader, relays, leaving)
- Players not ready stay behind at departure (s10-party replays whole)
- Real layer records for LayerChildInfo (MH3SP's placeholder showed 0/4 players)

### wire

- Typed OPN packets, round-tripped on the corpus
- Typed LMP packets; the macro lists every typed packet for the corpus test
- Typed FMP packets (107 types), round-tripped on the corpus
- Typed chat, tell, friend and black list packets, round-tripped on s07-s09

### world

- One quest per player (a second creation is refused); quests per city follow from it

