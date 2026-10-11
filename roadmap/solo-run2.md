# Solo run 2

The second part of the solo run (2026-10-11, from 10:28; same rules and deadline as [`solo-run.md`](solo-run.md)),
on `feat/solo-run2`, branched from `feat/solo-run`. Its scope: the user's notes (`../notes.md`), in this order.

1. **The configuration from a TOML file**, secrets also as `__file` (the user's answer to the configuration's shape).
2. **In-game commands** in the chat: `/tell <player> <message>` (to the target only), and for admins `/announce`,
   `/stop`, `/kick`, `/bot add`; `/ban` and `/whitelist` need the schema's `access_rules` (S5).
3. **The API** (the core is the only one to reach the database; the CLI and the website are its clients), with the
   `tokens` of `schema.md`, in a `shared` crate for the types and a `cli`, if time remains.
4. The website (SvelteKit), last, if time remains.

Provisional calls go to [`questions.md`](questions.md), the account to the report's second part.
