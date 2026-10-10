# Experiment: end-to-end tests on Dolphin

**Status: draft (2026-10-10), decisions open.** Before M6.

Console sessions are the only proof that the game agrees with the server, and today they need a person at the
controls for every run. Goal: scripted Dolphin runs on the lab Mac, started from the terminal, that drive the game
through a scenario and check it against the server, so a session that was checked once can be checked again
(a release check, regressions).

## Done when

1. One command runs a two-console scenario unattended (both from their save state into one city, one quest departed)
   and reports pass or fail, with screenshots on failure.
2. It fails when the server misbehaves (e.g. a broken city join) and passes on `main`, three runs in a row.
3. Adding a scenario means writing its menu path, not new tooling.

## What exists

- `lab/play.sh`: Dolphin per profile (muted, captures on), booting from a save state (the Village/City choice).
- `lab/shot.sh`: a screenshot of the game window.
- The server's log already names the steps (`entered gate`, `created city`, `quest departs`).
- Not in the game: a way to ask Dolphin where the game is (no scripting in mainline Dolphin).

## How

- **Inputs:** Dolphin's Pipes input backend: a named pipe per console, a script writes button presses into it.
- **Checkpoints: hooks in the server**, so the server and the test move together instead of guessing delays: the
  server tells the test when a step happens (entered a gate, created or joined a city, a quest departed, a console's
  error report), and the test answers by sending the next inputs. The test can also read the server's state (the
  health port's `/metrics`, extended if needed).
- **Menu paths:** the user maps the menus once (which screen, which buttons), written as data per scenario; screenshots
  check the screens the server doesn't see (before OPN, menus).
- **Starting point:** each run from a fresh copy of a known profile and its save state.

## Decisions (open, recommendation first)

- **X1 Hook shape:** a webhook: the server POSTs a small JSON event (`{"event": "entered_city", "capcom_id": …}`) to
  `TEST_HOOK_URL`; the test runner listens on it. Or: the server runs a script per event. Or: the test reads an event
  stream from the health port. The webhook keeps the server free of process handling and works from any language.
- **X2 Dev only:** the hooks off unless `TEST_HOOK_URL` is set, and IDs only in events (the logging rules). Or: a cargo
  feature, absent from release images. The setting is simpler; the feature guarantees production can't have it.
- **X3 Where the runner lives:** `lab/e2e/` (outside the repository, with the game image and profiles), a script per
  scenario. Or: in the repository (`tools/e2e/`), the game image still outside. The runner is code worth keeping:
  the repository.
- **X4 Language of the runner:** Rust (a `testkit` binary: it already speaks the protocol and can host the webhook) or
  a shell/Python script. Rust keeps one toolchain.
- **X5 The menu map's format:** a list of steps per scenario (`wait event`, `press A`, `hold up 2s`, `screenshot`),
  readable and written by the user.

## Work (once settled)

- [ ] The server's hooks (X1, X2), the events at the steps above.
- [ ] Dolphin's Pipes input per console in `lab/play.sh`; the runner pressing buttons.
- [ ] The user's menu map for the first scenario; the runner replaying it, waiting on hooks.
- [ ] The first scenario (Done when 1–3).
