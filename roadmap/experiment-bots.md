# Experiment: quest bots

**Status: draft (2026-10-10), decisions open.** Before M6, research first.

The newer Monster Hunter games let a player bring computer-controlled companions. Idea: a player alone could ask
for a bot in their quest, with a basic behaviour (follow the player, attack what they attack). Bots exist only in
quests, never in towns, gates or lists.

## What stands in the way

- **A quest is peer to peer.** The server only matchmakes; the consoles negotiate a link (NatNeg), then talk GameSpy
  GT2, carrying the game's own messages (positions, animations, attacks, damage, the monster). A bot must be a peer
  of that session: the server (or a process next to it) speaking NatNeg, GT2 and the game's in-quest messages.
  The relay already sits in the middle of relayed pairs' traffic, and our NatNeg decides who talks to whom.
- **Behaviour without the game.** Following means moving through a map whose geometry the bot doesn't have (copying
  the player's position with an offset would walk through walls); attacking means sending the hit and damage
  messages the game expects, with plausible animations. Who owns what (the host the monster? each player their own
  hits?) decides what a bot may claim.
- **What makes it possible:** `mhtri-decomp` is a matching decompilation: the game's whole code is readable, the
  session layer (`NetworkSession*`, `NetworkLayerPat`) and the hunter's and monster's code included.

## Phases, each ending with a go/no-go

1. **Research (decomp):** the in-quest message catalogue (who sends what, when, to whom), who is authoritative for
   positions, hits, damage and the monster; how a member joins the session; GT2's handshake. Output: a written map,
   and an estimate for phases 2–4.
2. **A bot that joins and stands:** a party member the consoles accept (in the party list, in the quest, standing at
   the camp), departing and returning with the party.
3. **A bot that follows:** positions near the player's.
4. **A bot that attacks:** hits on the player's target.

## Decisions (open, recommendation first)

- **B1 Scope:** quests only, at most one bot per player, asked by the player (how: a chat command in the city, e.g.
  `!bot`, or a fake member to invite; to find in phase 1). Never presented as a person: its name says it's a bot.
- **B2 Where it runs:** inside the server (one process, decision 1 of `architecture.md`), as a peer on the relay's
  side. Or: a separate binary using `testkit`'s console. Decided after phase 1.
- **B3 Fairness:** a bot's hunt counts for the player (rewards, HR) as a normal party member would; to check whether the
  game lets a non-console member affect that at all.
- **B4 Phase 1 first:** nothing built before the research says it's feasible and how big it is.

## Work

- [ ] Phase 1: research (B4), its map and estimate written here.
