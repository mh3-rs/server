# Packet encryption (experiment)

**Status: done (2026-10-10): one key per console, works on consoles. Off by default (E1).**

The game encrypts its LMP and FMP connections when the server gives it a key; MH3SP never does (it answers
`ReqCommonKey` with `AnsAuthenticationToken`, which the console accepts, so everything stays in clear), and neither do
we. Those connections are plain TCP and carry the support code (`AnsConnection`) and PAT tickets in clear: encrypting
them protects a player's credentials on the way. An experiment first: on only once consoles prove it.

## Exit criteria

1. A Dolphin console with encryption on goes from the title screen to a city, and a second one joins it.
2. Tier 1: our encryption and decryption round-trip, and decrypt what the console's algorithm encrypts.
3. Tier 3: the scripted login (OPN → LMP → FMP) with encryption on.

## What the game does (decomp, 2026-10-10)

- **The key** (`PatInterface::recvAnsCommonKey`, `PatInterface.cpp:3804`): after the vulgarity lists, on OPN (already
  under TLS), the console sends `ReqCommonKey` (empty); `AnsCommonKey` carries a byte array of up to 256 bytes, kept as
  the key for the rest of the game session. No key agreement: the server chooses it.
- **When** (`stepPatConnect`, `PatInterface.cpp:8025`): on every later connection, from its first frame, encryption is
  on if a key was received; never on OPN (`disableCrypt` for server type 2). LMP is type 0, FMP 1.
- **The cipher** (`PatCryptSetKey`/`PatCryptEncrypt`/`PatCryptDecrypt`, `network_pat_control.cpp:234–330`):
  Camellia with a 256-bit key (`Camellia_Ekeygen(0x100, key)`: the first 32 bytes), each 16-byte block on its own
  (ECB). Only the payload is encrypted; the 8-byte frame header stays in clear, its size field carrying the encrypted
  length. Encrypting: a 4-byte header (the payload length, big-endian u16, then two zero bytes), the payload, padding
  to a whole number of blocks (the padding bytes are whatever the working block held). Decrypting refuses a length
  that isn't whole blocks (error `0x80000000`, param `0x66`), then reads the length from the first block.
- **Confirmed on consoles (E4)**: the decompiled decryption seemed to write the payload 4 bytes further than the
  encryption reads it; the console reads it right after the 4-byte header, as MH3SP's branch did.
- MH3SP's branch `packet-encryption` (2023, unfinished) found the same cipher (`mh/pat_crypt.py`).

## Decisions

- **E1 Scope. Settled:** behind a setting, `ENCRYPTION=on|off`. **Off by default** (2026-10-10, after E4): it works,
  but stays an operator's choice for now.
- **E2 Key. Settled (2026-10-10, replaces a key shared by every console):** one key per console, drawn at its login
  (`AnsCommonKey`, OPN). It follows the console from connection to connection:
  - the server opens every LMP and FMP connection with a `ReqConnection` encrypted with the **newest key in transit
    from the console's IP**: the console's own in the usual case; with several consoles in transit at one IP it may be
    another's, which is harmless (the console ignores the opening's value: lab, every opening under a wrong key, a
    console logged in to a city). An IP without a key in transit is closed. An empty opening (no key needed) was tried
    first: the consoles refuse it (lab, 11612);
  - when a connection ends with `ReqShut` (the console moving on to LMP, FMP, another world server), its key goes
    **in transit**, with the console's IP, for 2 minutes at most (the ticket hand-off's time). Between two
    connections a console waits 2 to 5 s (measured in the corpus), so a login keeps a key in transit 10–15 s in all;
  - a new connection's first frame (`AnsConnection`, encrypted) is decrypted with the keys in transit **from its IP
    only**: usually exactly one (the console that just left), several only when consoles behind one router switch
    within the same few seconds, and then the one whose decryption reads as a valid `AnsConnection` (a wrong key
    gives garbage);
    that key leaves the transit list and stays with the connection. No key is ever tried outside its console's IP and
    that window. None fits: the connection is dropped.
  The first idea, one key for every console, is dropped: any console logging in could read another's traffic.
  Not chosen either: a TCP port per console (exact, but an extra port range for operators to open).
  A secret: never logged.
- **E3 Tests. Settled:** a round-trip test of the cipher, a test against the console's algorithm as decompiled, and the
  scripted login with encryption on. The corpus is in clear: replays run with it off.
- **E4 Lab. Settled:** one run with encryption on: login, a world server, a city, a second console joining. Getting past
  LMP's first frame proves the reading.

## Work

- [x] First, a recording to see what the console really does once it has a key (2026-10-10): `ENCRYPTION=on` makes
      OPN answer `ReqCommonKey` with `AnsCommonKey` (a random 32-byte key per process); nothing else encrypts yet,
      so LMP is expected to fail. The key is in Dolphin's OPN capture (its TLS dump). Test:
      `common_key_only_with_encryption`.
      **Result:** the console takes the key, then on LMP our first frame (`ReqConnection`, in clear) is refused: it
      hangs up without a word, error 11612. Having a key doesn't change who speaks first: the server's first frame must
      be encrypted.
- [x] `wire::crypt`: the cipher (Camellia-256-ECB from `openssl`), MH3SP's 2023 reading of the decryption (the length
      in the first 4 bytes, the payload right after; the decomp's 4-byte doubt stays for the lab). Tests: round trips,
      RFC 3713's Camellia vector, refusals.
- [x] `AnsCommonKey` typed; OPN answers it with the server's key when `ENCRYPTION=on`, else as today.
- [x] LMP and FMP: every payload decrypted on arrival and encrypted on departure (`net.rs`), from the first frame;
      an undecryptable frame drops the connection (`warn`). The test console encrypts like the game. Test:
      `encrypted_login` (OPN's key, both LMP visits and FMP; a console without the key dropped).
- [x] Lab run (E4, 2026-10-10): two consoles with `ENCRYPTION=on`, logged in, Valor1, a gate, one creating a city and
      the other joining it, chat sent: no error on either side, nothing in the server's log; every FMP payload in the
      capture is whole 16-byte blocks (263 of 263). MH3SP's reading of the decryption was right (the decomp's 4-byte
      shift was the decompiler's output, not the game).
- [x] Per-console keys (E2): the key drawn at `ReqCommonKey` (OPN), the transit list (`keys.rs`, filled at
      `ReqShut`), the opening with the likeliest key, the first frame decrypted with the IP's keys in transit; the decryption
      also checks the header's two zero bytes. Test: `encrypted_login` (two consoles behind one IP, crossing from OPN
      to FMP, one opening with the other's key; an address without a key closed).
- [x] Lab (2026-10-10): the empty opening refused by both consoles (11612, nothing sent back): replaced by the
      opening with the IP's newest key in transit. Nothing else ties two connections of a console together: it
      never speaks first (MH3SP waited 1.5 s), and each connection comes from a new port (shared pool; a router rewrites
      them anyway).
- [x] Lab (2026-10-10): per-console keys, two consoles one after the other into one city, then three logins of both at
      once (same IP): no error. Then one console with every opening under a random key (a local build): logged in to a
      city: a wrong key in the opening is harmless (the console ignores its value). The server still opens with the
      likeliest key, the console's own in the usual case.
