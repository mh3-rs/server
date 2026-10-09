# Unknown fields in the packets the server sends

**Status: report (2026-10-09), fixes applied (M5).**

Why: `AnsLayerChildInfo`'s leading `unk: u16` turned out to be the layer index the console asked about (0 = "my own
layer"); we answered MH3SP's constant 1, so the console filed the city's record as another layer's and Local Info
showed 0/4 (fixed in M3). This checks every other unknown field the server sends (27 fields in 25 packets) against the
decomp (RMHE08, `mhtri-decomp/src/Network/`): what the console does with each.

**Result: no other field repeats that bug.** Each is either read and dropped by the console, or passed on and ignored,
except `NtcCircleLeave`'s, which the console uses and where our value is consistent. One routing issue was found on
the way (`NtcUserBinaryNotice`, below).

Paths: PI = `PatInterface.cpp`, NLP = `NetworkLayerPat.cpp`, NSMP = `NetworkSessionManagerPat.cpp`, NCP =
`NetworkCommunityPat.cpp`.

## Used by the console

| Field | What the console does | We send | Correct | Name | Confidence |
|---|---|---|---|---|---|
| `NtcCircleLeave.unk` (u8) | PI:5072 → `removePlayerRecord(slot, kind == 2)` (NSMP:2975): 2 takes the player off a "counted players" tally, which `NtcCircleJoin.state` 2 adds to and circle field `0x08` sets | 0 | the leaving player's `NtcCircleJoin.state` | `state` | proven; what "counted" shows in game is unknown |
| `NtcUserBinaryNotice.unk` (u8, from `ReqUserBinaryNotice.unk`) | receivers ignore it (NLP:3119, NCP:2209); the sender sets it 0–4 (NCP:1126): **1 = for one named user** (the request then carries that user's Capcom ID), others send an empty ID; the corpus only shows 2 | the request's | the request's (fine) | `target` | proven for the sender; kinds 0, 2–4 unknown |
| `AnsLoginInfo.need_ticket` / `.unk` | PI:3848 → login state machine (PI:1495–1517): 1 continue to `ReqTicket`; 3 or 5 show the string to the player and stop; 2, 4, 6 another state (not traced) | 1, "dummy_data" | 1 (the string is ignored then) | `status`, `message` | proven |
| `AnsUserObject.need_ticket` / `.unk` | the same (PI:1672): 1 to reach `ReqTicket`; the string goes to the message buffer (PI:3917), unused when 1 | 1, "Unk_UserObj_str" | 1 | `status`, `message` | proven |

## Read and dropped by the console (any value works)

| Field | Evidence | We send | Likely meaning | Name |
|---|---|---|---|---|
| `AnsLayerChildListHead.unk` | PI:4484, NLP:2570 uses only the total | 0 | the request's `first` | `first` |
| `AnsLayerUserSearchHead.unk` | PI:4698, NLP:2757 | 0 | the request's `first` | `first` |
| `AnsLayerUserListData.unk` | PI:4663 drops it | 1 | the request's `first` | `first` |
| `AnsLayerUserSearchData.unk` | PI:4710 | 0 | the request's `first` | `first` |
| `AnsCircleListLayer.unk` | PI:5232; NSMP:2426 uses each circle's own number | 0 | first index | `first` |
| `AnsFriendList.unk`, `AnsBlackList.unk` | PI:5827, PI:5865 | 0 | the request's `first` | `first` |
| `AnsFmpListHead(2).unk`, `AnsFmpListData(2).unk` | PI:3938, PI:3950 (the `…2` opcodes share the handlers) | 0 | first index | `first` |
| `AnsLayerMediationList.unk` (u8) | PI:4925; the request is `(mode, max count)`, `(1, 32)` (PI:2932, NLP:3980) | 0 | the request's mode | `mode` (and the request's fields `mode`, `max_count`) |
| `AnsBinaryVersion.unk` (u8) | PI:4190; only the version is used | 0 | the request's binary kind | `kind` |
| `AnsVulgarityInfoLow.unk` | PI:4146 | 1 | unknown | `unk` |
| `AnsUserObject.unk2` (u32) | PI:3910 | 0 | unknown (MH3SP: "unused") | `unused` |
| `NtcCircleHost.unk` (u32) | PI:5379; the handler (NSMP:2540) uses only the leader's slot | 0 | the circle id (`NtcCircleJoin`/`Leave` carry it there) | `circle` |
| `NtcCircleBreak.unk` (u32) | PI:5099; the handler (NSMP:2705) only marks the party lost (error `0x80050037`) | 0 | the circle id (inferred) | `circle` |
| `NtcCircleKick.unk` (u8) | PI:5314; the handler (NSMP:2706) posts error `0x80050036` | 0 | a kick reason (unknown) | `kind` |
| `NtcLayerChat.unk` (u8, from the sender) | receivers drop it (PI:4817); senders set the chat channel (NLP:~5544: 1, 2, 3) | the sender's | the sender's (fine) | `channel` |

## Not covered by the decomp

`AnsSubTermsInfo { unk1, unk }` and `AnsSubTerms.unk`: EU-only packets (`0x6213`/`0x6214`), absent from RMHE08's
packet table. Inferred from the corpus: `unk` selects which sub-terms (language or region?) and is echoed in every
reply (we echo it: right); `unk1` is the sub-terms version the console sends back in `ReqSubTerms` (we send the terms'
version, like MH3SP). Names `version`, `index`; confirming needs the PAL `main.dol`.

## Fix proposal

1. **Rename** (no change on the wire, replays untouched), in `crates/wire/src/packets.rs` and the handlers:
   - `first`: the eleven list fields above;
   - `AnsLayerChildInfo.unk` → `layer` (and `ReqLayerChildInfo.unk` → `layer`), the M3 finding;
   - `status`, `message`: `AnsLoginInfo`, `AnsUserObject` (`need_ticket`, `unk`); `AnsUserObject.unk2` → `unused`;
   - `mode`: `AnsLayerMediationList.unk`, and `ReqLayerMediationList`'s `mode`, `max_count`;
   - `kind`: `AnsBinaryVersion.unk`, `NtcCircleKick.unk`; `channel`: `NtcLayerChat.unk`; `target`:
     `ReqUserBinaryNotice.unk`, `NtcUserBinaryNotice.unk`; `state`: `NtcCircleLeave.unk` (and `NtcCircleJoin`'s if
     typed `unk`); `circle`: `NtcCircleHost.unk`, `NtcCircleBreak.unk`;
   - `AnsSubTermsInfo`/`AnsSubTerms`: `version`, `index`, marked as inferred.
2. **Values: keep MH3SP's.** The console drops them, so changing them (echoing `first`, the circle id) would only add
   replay divergences without a visible effect. Exception, cheap and faithful: none needed now.
3. **Fix the routing of `NtcUserBinaryNotice`** (`fmp.rs`, `ReqUserBinaryNotice`): with `target` 1, send the notice to
   the named player only (if at the same place), not to everyone there. Not seen in the corpus (always 2): a test with
   `target` 1.
4. **Keep in mind** (no change): `NtcCircleLeave.state` must mirror the `NtcCircleJoin.state` sent for that player, if
   we ever send 2 on join or circle field `0x08`.
