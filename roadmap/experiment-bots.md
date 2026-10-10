# Experiment: quest bots

**Status: sprint 1 (A1–A4) done (2026-10-10); sprint 2 (A5–A8) agreed**; sprint 3 (part B, the companion) after it.

A player alone in a quest can be joined by a bot run by the server: it follows the player, tries not to die, and
attacks what the player attacks, as the companions of the newer games do. Bots exist for quest parties only: never
in world servers or gates, never walking in a city; in the city only as the party's P2P session needs (a place among
its 4). This repository keeps its decisions in `roadmap/` (lasting ones in
`architecture.md`). The end-to-end runner is paused (`experiment-e2e.md`), so each step here is checked on consoles
by hand, like the usual lab runs.

## What exists

- The server already speaks every part a peer needs but the last: PAT (parties: `CircleJoin`, `NtcCircleJoin`,
  `CircleUserList`, departure), NAT negotiation and the relay (M4), and it sits in the middle of relayed pairs' traffic.
  Missing: GameSpy GT2 (the P2P transport) and the game's own in-quest messages.
- `mhtri-decomp`, a matching decompilation: the game's whole code, its session layer and the hunter's and monster's
  code included.
- **What the decomp says** (research, 2026-10-10, read in code, not yet checked live):
  - **One P2P session per city, reused by the quest.** A console opens it on entering a city (4 slots); a party's
    traffic goes to its members' slots in that city session (`NetworkSessionManagerPat::mapId`,
    `broadcastPlayerSlots`). So a party member must be a GT2 peer of every other member, in the **city's** session.
  - **A member only on the PAT side gets partway, not in:** the PAT-side checks (`NtcCircleBinary` user values, the
    "linked" flag) come through the server and can be faked, but a member's P2P status only becomes "connected" when
    its session is established; departure waits for every member to be connected or failed, and a failed peer is
    dropped or kicked (`moveStartSession`, `checkMemberSessions`).
  - **What a peer speaks:** NAT negotiation (M4); a GT2 connect carrying a 20-byte profile equal to the receiver's
    (match id and parameters the server already hands out); a handshake (hello, hello reply, done; pings every second;
    close); a session establish (the expected address record, a nonce); framing: a 22-byte header (version, length,
    sequence numbers, nonces, a CRC-16/CCITT, two key bytes) and a payload under a rolling XOR with the key in the
    header; messages of at most ~0x200 bytes. Every game message starts with `{0, from slot, to slot, kind}`:
    slots 1–4 players, 5–8 monsters, 9 monster control, 10 effects, 13 quest events.
  - **In a quest**, each console sends only for its own hunter (state: position, area, sub-area, animation, HP,
    weapon; position; attack/hit effects; item hand-overs). Each monster has an **owning player** who sends its state
    (position, area, target point, bound player, HP, part damage and breaks) and the others overwrite their copy. The
    party leader decides the quest's events (spawns, supplies, monster control).
  - **Authority:** a hunter's console decides the damage its hunter takes and reports its faints. **How a hunter's
    hits reach a monster it doesn't own is not decompiled** (probably a quest-event message, family 13): the open
    question that decides whether a bot's attacks count.
  - **Vision:** a member receives the other hunters' position, area, animation and HP, and each monster's position,
    area, target, bound player, HP and parts. Map geometry never travels: only area numbers.
**Also for anti-cheat (M9):** the in-quest message catalogue, who claims what (damage dealt, damage taken, faints), and
the game's numbers (a weapon's damage) are what anti-cheat needs too: a hunter claiming 1000 damage with a weapon
rated for 10 is a red flag. The relay sees relayed pairs' traffic; the bot would see its party's.

### How a peer joins a city's session (research on `s04`, 2026-10-10, verified on the recording unless noted)

For A1. Decomp paths under `mhtri-decomp/src/Network` unless noted.
- **PAT order** (a host dev1, a joiner dev2): `LayerDown`, `LayerChildInfo`, `LayerUserList`, `LayerHost`; the server
  sends dev1 `NtcLayerIn`. Then dev2 sends `NtcLayerBinary` **kind 6**, its peer record (`sendPeerRecord`,
  `NetworkLayerPat.cpp:1266`): items `{index, type, value}` (type 1 u8, 2 u16, 3 u32): kind 6, **peerId**, **mode**,
  **session**, **port**. The receiver checks the sender is a layer member first (`findFriendByUserId`): `NtcLayerIn`
  (or the user list) must come before. dev1 answers **kind 7** (its own record) through `NtcLayerBinary2`, and opens a
  session slot (`connectPeer`). The host then sends **kind 8**, a pair's state (from, to, 2 negotiating); only the
  session host starts pairs (host with each member, then members a<b); the "to" side reports the result (1 ok, 2
  negotiating, 3 aborted, 4 link lost). After the link: kind 5 from the joiner, kind 4 from the host (flags 0). About
  10 s later, the hunter's town appearance and positions as `NtcLayerBinary` user fields: **town presence is all PAT**.
- **The record's values:** peerId = `AnsUserSearchInfoMine` 0x0f XOR `rand()<<16` (never checked by the receiver: a bot
  invents any non-zero u32); mode = `AnsUserSearchInfoMine` 0x10, used only to skip NatNeg when two modes are **equal**
  (a LAN shortcut): the bot's must differ from every console's; session = the first word of the match options (the lab
  host's IP; meaning unverified, only used when modes are equal); port = the console's game (GT2) socket's port.
- **NatNeg:** cookie = from.peerId XOR to.peerId; "from" (the host side) is client index 0 and listens, "to" is index 1
  and connects. Pings: type 7, 20 bytes, every 700 ms from the game socket; a console moves on after a ping with
  "got your data" = 1, waits 5 s, reports, then GT2. **For the bot**, our NatNeg skips its INIT: the server sends the
  console a CONNECT with the bot socket's address and answers the console's pings from it.
- **GT2 connect:** header `fefe type u16 serial u16 expected` (1 client challenge, 2 server challenge, 3 client
  response, 4 accept, 5 reject, 6 close, 7 keepalive; 0x64 ack, 0x68 closed). to→from: challenge (32); from→to:
  response (32) + challenge (32); to→from: response (32) + **profile (20)**; from→to: accept; then ack. The challenge
  and response algorithm is decompiled (`DWCi/dwc_nasfunc.c:2481`, `:2500`; a secret at line 164); a reimplementation
  reproduces both recorded responses. **The profile**: five big-endian u32: the "to" peer's peerId, the FMP server's
  IP and port (as the console got them in `AnsFmpInfo`), the FMP index, then gate<<16 | city (that order is a guess:
  every recording is 1/1). The listener compares all 20 bytes. At most 3 GT2 connections and 4 session slots: the
  bot needs a city of at most 3 consoles.
- **Connection handshake:** a datagram is `[u16 len][22-byte frame][payload]` repeated, then `0000`; the frame: 0x0101,
  length, seq, ack, src nonce, dst nonce, `04`, flags (0x10 connection, 0x20 session; 1/2 channel 0/1; 4 MTU probe; 8
  out of band), CRC-16/CCITT over the plaintext, two XOR keys. Hello `0x81` (resent every second) carries the sender's
  channel sequence numbers and a 22-byte record: kind 3, its Capcom ID (NUL-padded to 10), its session nonce, its
  session channels' sequence numbers; answered `0x82`, then `0x83` on channel 0 (event 1). Reliable channel 0 is acked
  with empty frames. Pings `0x91` index, u32 ms, answered `0x92`, every second each way; MTU: a 1028-byte probe
  answered `0x95 00000400`. Timeouts: 20 s (auth, silence, host).
- **Session establish:** the receiver compares the hello record's ID with the slot's PAT member (**"auth error"** on a
  mismatch: the bot's Capcom ID must be the one the PAT side announced); the nonce (`rand()+time`, never 0 or −1) and
  start sequences are stored. Session frames carry both nonces. In town, P2P is only session op 6 every 0.25–1 s,
  acks, pings and one MTU exchange; game messages start with the quest. Leaving: `0x85` on channel 1, then GT2 `fefe 68`.
- **Open:** the bot is usually the "to" side (the GT2 client); the city host role must never go to the bot (the host
  starts every pair); whether consoles accept a member with no town appearance fields is untested.

## Settled so far

- **Only in parties.** The server joins the bot to a party directly: a party member to the PAT side, a peer to the
  consoles (NAT negotiation and P2P, from the server). Nothing of the world server or gate is simulated for it;
  of the city, only the place its session needs: what the consoles need to accept it.
- **Identity:** its name says it's a bot; it lives in memory only (nothing in the database), for the quest's length.

## Findings and features

### Q1. Do its attacks count? — answered: yes

A bot attacks by being a hunter the owner's console sees attacking: next to the monster, facing it, playing its
weapon's attack animations. The owner's console decides whether each swing connects and how much it takes off, by the
game's own numbers: the bot can't hit harder than its weapon (nor cheat), and the rest of the work is making it stand
and swing where a hunter would. Small monsters are local to each console (only their deaths travel): the bot can't
hurt them, and they can't hurt it.

**Experiment results (2026-10-10, throwaway decoder outside the repository):**
- The P2P traffic decodes completely (every frame's CRC checks): GT2 headers only on its handshake and close; game
  frames as the decomp says (a u16 length and a `0000` trailer around each, not in the decomp units read; the CRC-16
  is over the plaintext). In-hunt chat is a session message carrying the sender's name: never to be logged.
- The recorded quests (`s05`, `s10`, `s11`) had no hit: everyone stayed at the camp.
- **A lab hunt** (two consoles, small monsters killed by each, both hit): **small monsters are local to each
  console.** No hit, damage or HP of theirs travels; only a kill, as a monster-control event the killer broadcasts
  and the other console echoes. An attack travels as the attacker's state message (its animation), nothing more. A hunter
  hit sends its own new HP (absolute) in its state message. Positions come at 2 Hz, moving or not; a state message
  on every animation change. Player message kind 3 is an area change (not an attack), kind 5 the quest spawn.
- One enemy was synced in full there: a 110 HP roamer nobody fought.
- **A lab hunt of a Great Jaggi** (2026-10-10, both hitting, dev1 carted, then abandoned) **answers Q1: a bot's attacks
  can count, by the game's own rules.**
  - A large monster has **one owning console**, which sends its state (position, action, target point, the targeted
    hunter's slot, HP and part durability, absolute values) on each of its action changes; the others overwrite their
    copy. Ownership moved from the host to dev2 when dev2 **entered the monster's area** first (the host sends the
    monster's "target" message naming the new owner), not when it attacked; dev1 entering later didn't take it back.
  - **The non-owner's hits counted with nothing sent but its own state**: dev1's attack animations and position, and
    the owner's next state message carried the lower HP (0.02–3.5 s later). Nothing on the wire carries a damage
    value: the owner's console applies the other hunters' replicated attacks to its copy of the monster, with the
    game's own damage (weapon, hitzones).
  - The monster attacking: its state's target point is the victim's position and a byte names the targeted hunter;
    the victim's console sends its own new HP. A faint: the hunter's state with HP 0, and two quest messages console
    to console; the server sees nothing of it.

### Q3. The features, in two parts

**Part A, a hunter that plays by the game's rules** (technical; no intelligence, every action commanded by hand):
until a feature exists the bot doesn't do it (until it receives damage it is invincible: a feature not built yet,
not a decision).

| # | Feature | What it takes (from the research) | Open |
|---|---|---|---|
| A1 | **Join** the party | PAT: party member (`NtcCircleJoin`, `CircleUserList`); P2P: NatNeg, the GT2 connect (its 20-byte profile), the connection handshake, the session establish, keepalives (pings ~1 Hz, session opcode 6), in the city's session (Q2) | how the consoles take a member that never walked in the city |
| A2 | **Exist** in the quest | the spawn (player kind 5), its state (kind 1) and position (kind 2, 2 Hz), a weapon and an armor | none known |
| A3 | **Move** | go to a position (positions at 2 Hz, state on each change of animation), change areas (kind 3) | walking speed and animations |
| A4 | **Ownership** | a monster's owner runs its AI: the bot must never end up owning one. Ownership goes to a hunter entering the monster's area (the owner or leader sends a target message naming it) | what triggers it, whether the bot can avoid it (not announcing its area change, holding back its position while alone) or hand it back: to play with in the lab |
| A5 | **Attack** | perform a weapon's attack at a position, facing a direction (kind 1 acts); there is no damage on the wire: the monster's owner decides whether it connects and how much it takes off | each weapon's attack act numbers |
| A6 | **Receive damage** | the owner's state names the targeted hunter and the aimed point: the bot computes the hit it takes (reach, damage) and sends its new HP (kind 1 act 6) | a monster attack's reach and damage (not on the wire) |
| A7 | **Faint** | HP 0 (kind 1), the two quest messages (family 13 kinds 6, 7), back at the camp | the family 13 handler isn't decompiled: a capture covers it |
| A8 | **Leave** with the party | the end-of-quest state (act 9/4), `ReqCircleMatchEnd`/`Leave`, the GT2 close | none |

**Part B, the behaviour** (drives part A): follow the player, place itself around the monster, pick the target (the
player's), chain attacks into combos, dodge what targets it. Its own sprint, once part A works.

### Later points (not this sprint's go/no-go)

- Armor sets: a few, matched to the player's level (neither much lower nor much higher).
- How a player asks for one: the website, or chat commands (`/bot`, global roadmap's future steps).

### Research for A5–A8 and part B (2026-10-10)

Six reports, outside the repositories (they quote the decomp and the captures at length): `lab/research/sprint2/`:
`attacks.md` and `damage.md` (the decomp), `captures/report.md` (the long hunt and the two Great Jaggi hunts, its
scripts beside it), `gamedata/report.md` (the disc, its scripts beside it), `gameplay.md` (Tri's rules, weapons,
armour, monsters, from the web), `ai.md` (companion AI, the state of the art). Decomp paths are `lab/mhtri-decomp`'s;
"recorded" means seen in the captures. Read in code or recorded unless marked.

**For A5–A8 (the body):**
- **An attack is an act the owner replays** (`Pl_net_recv_state`, `pl_frame_sync.cpp:606`; `fn_8025F088`,
  `pl_act_step.cpp:3272`): a remote hunter's copy plays the act's motion every frame and pushes its attack entries
  into the shared hit list, so the hitbox and its timing come from the motion on the owner's console. No combo
  chains on a copy: each swing is its own kind 1, and a new act cuts the previous one. No hit confirmation exists.
- **What the damage reads**: the weapon class, weapon, armour and skills from the bot's PAT profile (byte 0x05 the
  class: 0 GS, 1 SnS, 2 hammer, 3 lance, 4–6 bowguns, 7 LS, 8 switch axe); from every kind 1 the hunter's attack value
  (0x34, f32, trusted up to 700: an anti-cheat hole for M9) and sharpness colour (0x30). 0x26 is the act's random roll
  (crits, status procs, `pl_act.cpp:7646`): it must stay random.
- **Sword and shield, recorded** (both hunters, the starter weapon): 22 three-slash combos `4/0` +0.50 s `4/1`
  +0.40 s `4/2` +1.97 s `0/0`; a single `4/0` back to `0/0` in 1.10 s; `4/15` an ender; the approach `1/0` (moving,
  drawn) 0.40 s before `4/0`; draw `0/1`, sheathe `0/3`, roll `0/8`, guard 5/x, potion `0/53` then `0/57`. A combo
  moves the hunter 50–90 units forward. About 1.5 HP off the Jaggi (2160 HP) per act with the starter. Other weapons'
  act numbers differ (one number, a different move per weapon; no named table in the decomp): a capture per weapon.
  Bowgun shots: kind 9 carries the aim; whether bullets travel is a capture.
- **A monster's state** (group 5 kind 1, 108 bytes, on action changes only, 1.7–3.5 s apart): action 0x2A/0x2B
  (07/xx attacks, only while it targets a hunter), target mode 0x32/0x33 (1/2 a hunter), **targeted slot 0x35** (64
  of 64 recorded states), target point 0x18, HP 0x48, eight part values 0x56 (an HP drop equals theirs). Reach and
  damage never travel (per-species tables in `em_data.rso`, not decompiled).
- **A hit taken, recorded**: lands 0.6–1.1 s after the 07/xx state, on the targeted hunter, the target point 0–360
  units from it; the Great Jaggi's 07/03 took 17–24, 07/01 and 07/02 30, 07/05 24, 07/00 49 (of 100, starter
  armour). Reactions 6/2 (knocked back, 1.5 s), 6/3 (down, 1.7 s), 6/26 (big, 2.7 s), 6/0 and 6/1 (flinch); param 12.
  Only the victim's console changes its HP (`Pl_master_ck`); nothing checks a peer's HP (no kick for a lie). The
  damage formula (defence, guard, elements, statuses) isn't decompiled.
- **Faint, recorded**: the victim's state 8/0 HP 0 (param 32) and family 13 kind 6 from its slot; the host answers
  13/7 naming it, and every console takes the faint penalty off the quest's reward on that 13/7
  (`quest_reward_faint_penalty`, `quest_entry.cpp:1898`); reward 0 fails the quest (usually 3 faints,
  `penalty_per_cart` in the event files). Positions at 2 Hz with HP 0, kind 3 to area 0 with 0x1F = 2 at +10 s, idle
  at the camp with full HP at +17 s. A carted owner hands its monster on if a hunter is in the area.
- **The end**: the host confirms the objective (13/0x16) and the quest's end (13/0x19, a state byte: 3 complete, 5
  failed, a guess); abandoning is state 9/4, then `ReqCircleMatchEnd`, `ReqCircleMatchOptionSet` 01030100,
  `ReqCircleLeave` (the others follow 5–18 s later, with no 9/4). Nothing found waits on every member; the city's P2P
  session stays up. A completed quest with two consoles isn't recorded.
- **Party heals** heal each console's own hunter only: the bot restores its own HP when a hunter near it uses one
  (which acts: a capture).
- Family 13 seen besides: kinds 10, 11 at the quest's start, 1 and 12 around the supply box, 19 (a subquest?).

**For part B (the behaviour, sprint 3):**
- **Tri's own companion**: Cha-Cha's AI (`src/ai/ai_npc.cpp`) is a hierarchical state machine: behaviours by distance
  band, rings around its target, frame timers (a 450-frame approach, a 90-frame guard within 2048 units): constants
  to borrow. The monsters' AI (`src/enemy/emNNN_ai.cpp`) holds their attacks' reach and timing.
- **Shipped companions** (`ai.md`): state machines (Ellie, Left 4 Dead's bots with their reason strings, Cha-Cha),
  utility scoring only for real choices (where to stand, which attack); positions in a ring around the player or
  slots around the target, never in front, never in the player's way; hysteresis (two thresholds per band: the
  follow's oscillation); jitter rather than added delay (our 0.5–1 s already exceeds human reaction); deliberately
  less effective than a player and with switches for "spotlight" actions (Wilds' support hunters). Learned or LLM
  control doesn't fit (no simulator, seconds per decision); an LLM could turn chat into commands. The crates are
  Bevy-bound (`big-brain`) or more code than a `match` (`bonsai-bt`). Testing: a pure `tick(world, now, events) ->
  actions`, two- or three-character scenarios, the captures replayed with invariants.
- **Tri's rules** (`gameplay.md`): survival before damage (a faint costs the party); weapons don't hurt allies but
  flinch or launch them (the trips that cart players): never swing with the player in the arc; SnS or lance suit a
  bot (short, safe), hammer and great sword launch allies; online HR bands (★1 HR 1–8 … ★6 HR 51+) and the armour
  sets of each, for gear matched to the player; fights in water need depth; Lifepowder, traps, capture (killing a
  capture target fails it).
- **The game's data** (`gamedata/report.md`, the PAL disc, `nodtool extract`): not MT Framework, nothing compressed.
  Every area's **ground and wall collision** (`15/mNNd/*/…-atari.sch`, a simple format, decoded, its heights matching
  recorded positions: navigation becomes possible); each area's start point; monsters' base HP and weapons' raw in
  plain tables by export name. Hitzones, attack power per attack and armour are behind pointers (weeks, uncertain);
  timing and hitboxes are code; no table of exits (learn them from the hunters' area changes). `mh3-data` (MIT code,
  Capcom's numbers) has Tri's weapons, armour, HP, hitzones, motion values. Either would be read from a folder given
  at startup, never this repository.

## Phases

The research phase is done (above): its go/no-go is a go. Part A, then part B, each feature checked on consoles
before the next; this sprint's scope is to settle.

## Decisions

- **Three sprints** (2026-10-10): A1–A4 (join, exist, move, ownership), then A5–A8 (attack, receive damage, faint,
  leave), then part B (the behaviour). This file describes all three; each sprint takes its part.
- **Two roles, two binaries** (2026-10-10): **the server is the bot's body**: it does the NAT negotiation, GT2 and
  the session with the consoles (a P2P peer per bot, a UDP socket each), receives everything a quest member receives,
  and plays the bot's actions on the wire; it keeps the bot's party membership in memory. **The brain
  (`mh3sp-brain`) is the brain**: pure logic, it never touches the game's protocol, so it can be tested without a
  console and swapped. Rejected: the bot binary speaking GT2 itself (the protocol in two places).
- **A direct TCP tunnel between them** (2026-10-10): persistent and both ways (the server hands the bot a party to
  join; the bot reports back), rather than webhooks.
- **Chat commands** (`/bot`…) stay a future step (global roadmap).
- **Three repositories** (2026-10-10): `core` (this one, the server), `brain` (`mh3sp-brain`) and `tunnel` (the
  messages and framing both use), side by side under `~/git/perso/mh3sp/`. `core` and `brain` depend on `tunnel` over
  git, pinned to a commit (branch `main`); a change to the messages is a `tunnel` commit, then both sides move to it.
  `tunnel` is private: CI fetches it over HTTPS with `gitea_token` (a URL rewrite in the pipeline), the Dockerfile with
  a build secret. Rejected: one repository with three crates (one CI, but the user wanted them apart).
- **The tunnel** (2026-10-10): **one TCP connection, kept open, for every bot** (10 bots: one connection; each
  message names its bot); the server listens on localhost only, on a port set by a setting (`BOT_PORT`: no setting,
  no bots), and the brain connects. Messages are length-prefixed frames, each a serde value encoded in binary
  (which encoding: at the code, not now). Across it, see below.
  **A bot is named by its Capcom ID everywhere** (generated by the server at creation: the consoles, the server's
  party, the logs and the tunnel use the same one; a tunnel-only number was rejected: a second identity to map).
  **Events are per party** (several bots in one party isn't rare): each names its party by the server's quest
  identifier (world server, gate, city, quest slot), and the brain groups its bots by party. The messages (names and
  meaning; the fields at the code):

  | Direction | Message | About |
  |---|---|---|
  | server → brain, commands | create | a bot (its Capcom ID), its party, its profile |
  | | leave | a bot leaves |
  | | behaviour | a bot's behaviour: follow (whom), wait, passive, attack |
  | server → brain, events (per party) | quest started | the quest, its stage |
  | | hunter | a hunter's position, area, action, HP |
  | | area changed | a hunter entered an area (the game's player message kind 3) |
  | | monster | a monster's position, area, action, target, HP |
  | brain → server, actions | move | bot X to position Y |
  | | change area | bot X enters area Z (the server sends kind 3 for it) |
  | | attack | bot X performs an attack |

  Rejected: JSON lines (binary preferred), webhooks (one way only), a connection per bot.
- **Commands come from the server** (2026-10-10), so `/bot add` from a city's leader can follow later: in part A an
  operator command (`mh3sp bot …`, through the health port like `announce`) goes to the server, which sends it down
  the tunnel; the brain answers with actions. Rejected: commands typed into the bot binary.
- **The bot takes a place in the city** (2026-10-10): it's a peer in the city's P2P session (the quest reuses it), so
  it uses one of the city's 4 places; a full city has no room for a bot.
- **A generated identity** (2026-10-10): a Capcom ID no player can get, a name that says it's a bot, a profile the
  server builds; memory only.

- **A bot exists only in a party** (2026-10-10): it's ready from the moment it joins (it follows its leader; it'll
  stand by the departure once it can move), so the departure takes it along; when its party ends without it (the
  party broke, it was left behind) it leaves the city, its port and its brain with it. A bot alone in a city leaves
  too, and a bot never becomes a city's host (it can't host a session): with only bots left, the city has none until
  they go.
- **Lab findings, step 2a** (2026-10-10): the console accepts a bot's peer record, answers it (kind 7) and pairs with
  it through NAT negotiation, the server pointing it at the bot's socket. A finished ping gets no answer (GameSpy's
  rule; answering every one made the two ping each other forever). The city marks a party member from its town
  status (kind 6: flags 1 leader, 2 member, 6 member and ready, 0 out of a party; the bot sends it); the mark's colour
  (the quest's kind) comes from something the bot doesn't send yet (likely the town record consoles send on arrival,
  kinds 1 and 0x0c, or the P2P session). "[BOT] : [BOT] entered the city" is the game's own format (the decomp).
  Untested: a hunter joining a party after a bot may wait up to 30 s on it (`NtcCircleBinary` kind 2, answered kind 1
  by every member: the bot doesn't answer yet).

- **Step 2b, the bot in the city's session** (2026-10-10): after NAT negotiation the bot connects as consoles do (GT2,
  the connection handshake, then the session: acks, pings, the MTU probe, op 6 in town), reports its pairs (kind 8
  state 1 as the "to" side, then kind 5) and is removed if its session isn't up 20 s after joining. **Both GT2 roles**:
  the host pairs itself with everyone, and the other members by their order in the session, so a hunter arriving
  after the bot makes it the listening side. Rejected: refusing hunters into a city with a bot (breaks normal play),
  the listening side later (a known hang). The 20-byte profile is built from what the server knows (gate<<16 | city:
  a guess, checked in the lab in a gate other than 1). The code stays in `peer.rs`, the bytes in `wire::p2p`; tests
  drive the bot's task with the console side rebuilt from s04's frames. No town appearance yet (whether consoles mind:
  the lab). The quest (its loading sync, the camp) is step 3.
- **Open, for later (before step 3): the bot as a PAT client of our own server.** Today `add_bot` builds the bot's
  membership by hand (place, arrival, join, status, ready); the bugs found in the lab were all of that kind. Instead
  the bot would send a console's requests (`ReqLayerDown`, `ReqCircleJoin`, `ReqCircleMatchOptionSet`, its town
  data) through the normal handlers, from a session inside the server: every check and cleanup then covers it, and
  `add_bot`'s special cases go. Its cost: a session object handlers accept without a connection, and the console's
  order of requests in the bot. A first experiment: `ReqCircleJoin` driven by a bot session.

- **Lab findings, step 2b** (2026-10-10): the bot joined a city's session in gate 3 as the "to" side (the console
  accepted its GT2 profile: gate<<16 | city holds), about 2.5 s after joining; the town keepalives ran for 4 minutes
  without an error. A first run failed on a race (the console's first ping 9 ms before the host's pair announcement:
  the bot now connects once it knows both, in either order). Departing with the bot still hangs in the loading,
  without error. After the departure the console sent the bot nothing new over P2P, but it sent the party its
  `NtcCircleBinary` (items 1 = 3, 2 = 3), which it hadn't in the run without a session. Every member sends that same
  message after a departure and receives the others' (s05, s11): likely a party-level sync over PAT, which the bot
  neither answers nor sends (our relay drops the bot's copy). A joiner's kind 2, answered kind 1 by every member, is
  the same packet. Not tested on consoles yet: the bot as the listening side (a hunter entering after it).

- **Lab, into the quest** (2026-10-10): with the bot answering the party's kind 3 (`NtcCircleBinary`, PAT) and the
  departure's roll call (P2P, a session game message: the host sends `<index> 0d0a 00000003` every second until each
  member answers with its own index, then `0d0b`; s05, s11), dev1 departed and the quest started with the bot: its
  HP bar shown, its marker on the map in the camp, no body (it sends no spawn or position yet). The session index is
  taken as the party slot from 0 (a guess: the host is 0, the second member 1 either way). A departed quest passes to
  its next member: with the hunter gone the bot led a quest alone; a party with no hunter now takes its bots with it.

- **Lab, the bot moving** (2026-10-10): the bot spawned at the camp (its 0205 a copy of the host's 0105) and, with
  the brain following dev1 (`follow`, from the hunters' positions forwarded by the server), followed dev1 across the
  camp and into another area: its position message's area byte (0x1d) is enough to change area. It glides standing
  (no full state, player kind 1, with an action) and lags by up to a second, jumping (the hunter's position comes
  about twice a second; the bot repeats its latest every 0.5 s). The animation fields come from the long recording
  (`recordings.md`).
- **The long hunt, decoded** (2026-10-10, `recordings.md`; offsets from the game message's start, header included):
  - **Facing**: a big-endian u16, 0x10000 = 360°, `atan2(dx, dz)`: at 0x16 in kind 2, 3 and 5, 0x1a in kind 1.
  - **Animation**: kind 1 (sent only when the action changes) carries the act group (0x23) and number (u16, 0x24):
    0/0 idle, 1/1 walk (~440 units/s), 1/2 run (~730), 1/4 then 1/5 a run's start, 1/3 a stop (then 0/0 0.5 s
    later), 0/1 draw and 0/3 sheathe (byte 0x39 the weapon out), group 4 attacks, 6 hits taken, 8/0 a faint. Kind 2
    every 0.5 s, moving or not. A bot walks with: kind 1 1/1 (or 1/2) at the start, kind 2 every 0.5 s with the
    position advancing, kind 1 1/3 at the stop, kind 1 0/0 0.5 s later.
  - **Speeds**: the keyboard's on/off stick gave two speeds (walk, run); an analog stick likely gives speeds in
    between (not recorded: the bot uses the two).
  - **Area change**: kind 3 with the new area's fixed entry point, the area at 0x1d, 0x1f = 1 (walking in); then kind
    1 with param 0x0c at the real position.
  - **A non-owner's hits** on a large monster travel as nothing but its replicated attack acts and position: the
    owner applies them (its monster state's HP at 0x48 fell after the non-owner's combo). Ownership moved by area
    entry (5/2), and back when the owner carted out.
  - **Faint and cart**: kind 1 8/0 HP 0 and 13/6 from the fainting player, 13/7 from the host naming it; no kind 2
    for ~10 s; kind 3 with 0x1f = 2 (cart) at the camp's fixed point; kind 1 with full HP ~7 s later. No kind 5.
  - **An item**: no message of its own, only kind 1 acts (0/53, 0/57, then 0/0 with the HP restored).
  - **The quest's end**: abandoning is kind 1 act 9/4, then `ReqCircleMatchEnd`, `ReqCircleMatchOptionSet`,
    `ReqCircleLeave`; a completion and its rewards send nothing to the server.
  - **The game's watchdog**: a console counts 90 s of its own game time from the server's last line check (every
    30 s), then reports `NtcCollectionLog` {0x80000000, 107, 90} and leaves. Speeding Dolphin up (Tab) trips it:
    dev2 (sped up 2–3.9×) dropped at 19:40:36, dev1 later. Not a server problem; the lab must run at normal speed.
- **Following a hunter, a leash** (2026-10-10, kept as good enough; **to improve later**): every 0.5 s the brain
  compares the followed hunter's latest position (from its positions and full states) with where it last sent the
  bot. Within 250 units (2.5 m) it sends nothing, and the bot stops after 0.6 s; further, it steps toward the hunter,
  never closer than 250, by the distance left: a walk (440 units/s) under 150, a run (730) under 600, a sprint
  (1100) under 2000, a jump beyond (or into another area). The bot covers each step in the time since its previous
  target, faces it, and sends walk, run, stop and idle as hunters do. In the lab it follows across areas but lags and
  still slightly overshoots: the hunter's position is up to 0.5 s old (its console restarts its 0.5 s timer after
  each full state), plus the bot's own 0.5 s. Tried and dropped on the way (each worse in the lab): leading the
  hunter by its velocity (overshoot), the server copying the hunter's movement messages a second late (inside the
  hunter, frantic running on attacks), a breadcrumb trail 1 m behind (stop and restart between late positions), a
  time-based replay of the path (no better), and stepping every 0.25 s (no better).
- **A4, who owns a large monster** (2026-10-10, lab): with the bot left alone in the Great Jaggi's area (dev1, the host
  and owner, walking out), dev1's console handed the Jaggi to the bot at once (monster group 5, kind 2: owner slot at
  0x1e = 1, area at 0x1f = 1) and never took it back, dev1 back in the area included: the Jaggi froze (its idle
  animation looping) and ignored dev1's hits (only an owner applies them). A handover's layout (four in the long
  hunt): the sender's header, its own counter (bytes 4–7), the monster's position and facing, the new owner's slot
  (0x1e), its area (0x1f). An owner hands one back the same way: dev2, carting out, sent a 5/2 naming slot 0, and dev1
  answered with its own 5/2 and ran the Jaggi again. Handing it straight back to its giver started a ping-pong (the
  giver's rule hands it to whoever is in its area: 15 times a second while the bot was alone there). **Kept**: the bot
  keeps a monster it's handed and hands it to the first hunter in its area (lab: given at 23:14:55 as dev1 left,
  handed to dev1 at 23:15:41 as it came back, the Jaggi normal at once; two messages). Its limit: while the bot owns
  it the monster is frozen, so it doesn't roam to other areas as large monsters do. An alternative for later: the
  bot leaves the monster's area as soon as it's handed it (the giver then keeps it or hands it to a hunter).
- **Ideas, later** (2026-10-10): each bot of a party its own name (`[BOT] 1`, `2`, `3`: the game's name length to
  check); a bot answers its commands in the chat ("Following dev1", "Passive mode": a `Say` from the brain, sent as
  the bot's chat); a following bot could copy its hunter's actions (walk, run, stop: the hunters' full states reach
  the brain) rather than derive them from its own moves.
- **Behind a `bot` feature** (2026-10-10): the server's quest bot code builds only with `--features bot` (the
  tunnel dependency, `bots.rs`, `peer.rs`, `game/fmp/bot.rs`, the hooks in the handlers, the health endpoints and
  the CLI); the default build, the image included, has none of it, and its world rules about bots stay dormant.
  The tests run with it (testkit enables it); CI also lints the server without it.
- **How a player gets bots** (2026-10-10, the intended flow; its exact form settled with the chat commands): the
  player creates a quest, types `/bot add <1-3>`, the server creates that many bots in the quest, and the player
  departs with them. A bot never waits in a city outside a quest: it arrives to join one and leaves when it ends
  (cancelled, left behind, no hunter left) or when a hunter walks into the city.
- **A bot in the city like a hunter, until a hunter comes** (2026-10-10): the bot goes through the normal path
  (counted, listed, its arrival and quest lines in the chat: less code of our own, fewer bugs), and a hunter walking
  into its city sends it away, out of the city and its party, before the two can pair; a city's room is counted
  without its bots. So a bot is never a pair's listening side (that code stays tested, unused). Rejected, after the
  lab (2026-10-10): a bot invisible at the city level. (a) Without `NtcLayerIn` the console ignored the bot's record;
  (b) 50 ms after the bot's `NtcLayerOut` the console closed its GT2 link (a console drops a member that leaves); (c)
  hiding only what the server controls (lists, searches, counts) left the chat line and the console's own list:
  little gained for code of our own.

## Tests (sprint 1)

- The tunnel's framing and messages round-trip (unit tests); the server's side with a scripted brain: a bot
  created into a party shows as a member to `testkit`'s consoles (`NtcCircleJoin`, the party list), in the city's
  count; an action sent down the tunnel comes out as the bot's messages.
- The GT2 and session layers against the recorded traffic: the throwaway decoder's findings as unit tests (the frame,
  its CRC over the plaintext, the XOR, the hello/ping messages), and replaying a recorded handshake's answers.
- On consoles (by hand): the bot joins a party, departs with it, stands at the camp; then moves and changes areas on
  command; then the ownership experiments (A4).

## Not in this sprint

- A5–A8 (attack, receive damage, faint, leave: sprint 2) and part B (the behaviour: sprint 3).
- Armor sets matched to the player; how a player asks for a bot (`/bot add`, the website).

## Done when (sprint 1)

1. A command (`mh3sp bot add …`) puts a bot in a player's party; both consoles of the party (a player and a second
   console watching) show it as a member; it departs with the party and stands in the quest, then leaves with it.
2. The server forwards the quest's events to the brain, and plays the actions a minimal brain sends (a test tool
   sending "move to Y" and "change area" by hand); the consoles see the bot there.
3. A4: what makes a bot own a monster is known, and a way to avoid it works on consoles (or the finding that it
   can't, written here).
4. `scripts/ci-tests.sh` and CI are green.

## Outcome (sprint 1, 2026-10-10)

Built, each step checked on a console (the findings are in "Decisions" above, by date):
- **The body** (`crates/server/src/peer.rs`, `peer/link.rs`, `peer/body.rs`): a bot joins a quest's party through the
  normal city path, its peer record goes to the city, NAT negotiation pairs it, it connects (GT2, the connection
  handshake, the session) as either side, keeps the session alive, answers the departure's party sync (PAT kind 3)
  and roll call (P2P `0d0a`), spawns in the quest, walks, runs, stops and enters areas like a hunter, and leaves
  with its party. A pair not up in 20 s removes it.
- **The tunnel and the brain** (`../tunnel`, `../brain`): hunters' positions to the brain; `move`, `area`, `follow`
  (a leash: every 0.5 s a step toward the hunter, gait by distance, 2.5 m short) and `stay` back.
- **A4**: a bot alone in a large monster's area is handed it (and it freezes); the bot keeps it and hands it to the
  first hunter in its area.
- Along the way, in the server for everyone: only a quest's leader departs it or sets its details; a city never
  goes to a bot as host; a party with no hunter takes its bots with it.

Diverged from the plan:
- "Both consoles show it as a member" (done when, 1): a hunter walking into a city with a bot now sends the bot away
  (decided in the lab, "A bot in the city like a hunter, until a hunter comes"), so a second console never shares a
  party with one; it saw the bot leave instead. The bot as a pair's listening side, built and unit-tested, is
  therefore never used on consoles.
- Following took far longer than planned: five designs were tried before the leash ("Following a hunter, a
  leash"); it lags and slightly overshoots, and is marked to improve.
- Invisibility in the city was tried and dropped (the console needs `NtcLayerIn`, and drops a member that leaves).

Left for later (written in "Decisions" and the global roadmap): a better follow; the monster frozen while a bot owns
it (it can't roam; the bot could leave the area instead); the quest icon's colour; the bot's HR 0; names and chat
answers; the bot as a PAT client of our own server (before sprint 3); a hidden referee bot for anti-cheat (M9).

### Retrospective

- **Went well:** the recordings and the decomp gave every protocol step before it was coded, so GT2, the session,
  the roll call and the spawn worked on the first or second console try; bugs were found in the captures (the ping
  storm, the 9 ms race, the 1.3 s stale positions) rather than guessed; the pure `Link` and `Body`, tested with
  recorded messages, kept the console runs for what only consoles can tell.
- **Went wrong:** the follow behaviour was redesigned five times on impressions before its capture was read; the
  capture showed the cause (a console restarts its position timer after each action change) in one look. Two
  invisibility experiments the decomp already predicted to fail cost two console runs. Scripted text edits broke
  repeatedly on code rustfmt had reflowed. Every server restart disconnects the console, so each fix cost a full
  login.
- **Tested enough?** The P2P stack is tested against recorded messages and on one console; not covered: the bot's
  task as a whole (pairs, the 20 s guard, ownership choice) beyond the server test's join and leave, the listening
  side on a console, two hunters with a bot in one quest, a quest completed with a bot.
- **Next time:** read the capture before redesigning a behaviour; write the behaviour's rule down and agree it
  before coding it; check the decomp's verdict before spending a console run on an experiment.

## Sprint 2: A5–A8

**Status: agreed (2026-10-11)**

Part A's second half, as planned: the bot attacks, takes the monster's hits, faints and carts, and leaves with the
party, by the game's rules; every action still commanded by hand (typed into the brain), no intelligence. The
companion (part B) is sprint 3: its research is kept above ("Research for A5–A8 and part B"). The facts this sprint
builds on: the same section, "For A5–A8".

### Decisions

- **The server plays the game's rules, the brain only the strategy** (2026-10-11; architecture decision 11). The
  server knows how a move plays out on the wire (an attack's acts and their recorded timings), applies the hits the
  bot takes, keeps its HP, and faints and carts it, as it already walks it (`Body`) and hands monsters on (A4). The
  brain names moves (`Attack { bot, combo }`; typed `attack <bot> [1|3]`) and decides when. Rejected: the brain
  sending and timing acts (the game's mechanics on both sides of the tunnel).
- **A faint costs the party** (2026-10-11): the bot sends 13/6 and the host's 13/7 takes the penalty off the reward,
  as for a hunter. A bot is a large advantage; it can have a price. Rejected: a free cart (no 13/6).
- **Recorded numbers live in the code** (2026-10-11): the damage table and the combo timings are our measurements,
  like the templates in `body.rs`. The rule against game data is about not copying other projects' or the internet's
  content into the repository (`mh3-data`, the reports' sources).
- **The bot's HP goes to the brain** (2026-10-11): a strategy needs it (back off, sprint 3). The server sends the
  bot's HP when it changes, its faint and its return to the camp.
- **The bot as a client of our own server is its own sprint** (2026-10-11), after this one, starting by weighing
  whether it's worth it: the bot going through the normal handlers (every check and cleanup covers it, no
  duplicated logic or incoherent state) against its cost (a session with no connection, the console's order of
  requests in the bot).

- **An attack faces the large monster in the bot's area** (2026-10-11): its last state's position, straight ahead
  if there is none. The target is nearly always that monster; which monster to attack, when there's a choice, is
  strategy and joins the command in sprint 3. Rejected: a position in the command and the monsters' states sent to
  the brain now (sprint 3 adds them when the strategy needs them). In the lab the operator walks the bot to the
  monster (`move`), then types `attack`.
- **The damage it takes: the recorded rule** (2026-10-11): a large monster's 07/xx state naming the bot's slot (0x35)
  hits it 0.8 s later if it is still within ~350 units of the target point, for that action's recorded damage (the
  Great Jaggi's table first; a per-species default for an action not recorded, a capture per monster as we play
  them), with the reaction act by damage (6/0 under 10, 6/2 under 30, 6/26 above). Walking out in time is a dodge
  that works. Small monsters can't hurt it (local to each console). Rejected: the disc's attack power (1–3 weeks,
  uncertain; reconsidered if M9 needs it), never hurt or a fixed fraction (dishonest to the HP bar).
- **The starter gear, for now** (2026-10-11): SnS (class 1), the recorded starter weapon and armour, attack 60.0 and
  sharpness 2. A few sets for higher levels later (sprint 3 or after). Rejected for now: gear matched to the player.
- **Leaving is already handled** (2026-10-11): an abandon takes the bots with the party (sprint 1). Only a quest's
  end (completed, failed: the host's 13/0x19) is to check in the lab; code only if a console waits on the bot then
  (it would send 9/4 and the three requests as a hunter does).

## Tests (sprint 2)

- Body (pure, from recorded messages): `attack` comes out as the recorded acts with their timings, facing the
  monster; a 07/xx naming the bot hits it inside the radius and misses outside, 0.8 s later; HP, the reaction act;
  the faint (8/0, 13/6), the cart at +10 s, full HP at +17 s.
- Brain: `attack` typed becomes the tunnel's message (the existing typed-command test); the tunnel's new messages
  round-trip (the bot's HP, faint, return).
- On consoles, a Great Jaggi hunt with a bot, by hand: its combos lower the Jaggi's HP (in the owner's state); it
  takes hits standing in front, misses them when walked out; it faints and carts (the penalty taken); a quest
  completed and one abandoned with it.

## Not in this sprint

- The bot as a client of our own server (its own sprint, next).
- Part B (sprint 3): the companion's behaviour, gear matched to the player, other weapons, navigation, potions and
  heals, chat.

## Done when (sprint 2)

1. On consoles, a bot's typed combos take HP off a large monster it doesn't own.
2. It takes the monster's hits by the agreed rule, faints and carts as a hunter does, and the consoles show it.
3. A quest completed and one abandoned with the bot end for everyone, nothing hung.
4. `scripts/ci-tests.sh` and CI are green.
