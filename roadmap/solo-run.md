# Solo run

Claude works alone, with no one to answer, for a few hours. The rules below were agreed with the user on 2026-10-11;
they override the global CLAUDE.md where they disagree (deciding without the user, mostly). Start by reading this
file, then [`00. global.md`](00.%20global.md) and [`review.md`](review.md).

## Deadline

**Stop by Sunday 2026-10-11, 13:00 (CEST), whenever the run starts.** Check `date` between to-dos. From 12:40, start
nothing new: finish or set aside the current to-do, make sure the last commit is green, write the report.

## Scope, in order

1. **Cleanup and correctness**, every [auto] item.
2. **Safe to leave running**, if time remains.
3. **The deployment form**, if time remains: only what doesn't depend on the configuration's shape (a [user] item:
   write the proposal in the questions, don't implement it).

Not in scope: [lab] items (no console runs; add each check they need to `recordings.md`'s "Pending" instead), [user]
items (`TODOs.md`, the limits' values beyond proposed defaults, secrets, pushes), the Nintendo login, the patch.

## Decisions already taken

- **Decision 9**: the code changes to match the doc. A database error mid-game fails cleanly (an in-game message, the
  player stays connected); it no longer drops the player.
- **The schema**: apply what `schema.md` and the review say. Where the code has to diverge, update `schema.md` in the
  same commit and say so in the report.
- **Unknown requests**: identify each of the 33 untyped request ids yourself (the decomp first, then MHTrIDA,
  MHTriServer, MH3SP) and give each a safe default. Record each one (id, name, evidence, the answer chosen) in the
  commit and in `docs/`; one you can't identify keeps today's behaviour and goes in the questions.
- **Limits** ("Safe to leave running"): pick defaults, mark them provisional in the code's doc comment and in the
  questions.

## How to work

- **Branches**: `feat/solo-run` in `core` and in `docs`, from `main`. One commit per to-do, so each can be reviewed
  or reverted alone. Never merge into `main`, never push.
- **Green before every commit**: `scripts/ci-tests.sh > "$TMP/ci.log" 2>&1 && git commit -F ...`. After a schema
  change: `scripts/dev.sh --reset`, `scripts/sqlx-prepare.sh`. A to-do that can't get green within reasonable effort
  is reverted, not committed red; it goes in the questions.
- **Blocked**: write the question in [`questions.md`](questions.md) (what, why it blocks, the provisional answer if
  any) and move to the next to-do. Never wait for an answer.
- **Provisional calls**: allowed when cheap to undo; each one listed in `questions.md`. A call that would be costly to
  undo is a question, not a decision.
- **State lives in files**, not in memory: tick each to-do in `00. global.md` in its commit; decisions go into `docs/`
  in the same commit as the code.
- **Before each commit**, review it as a critic: edge cases and failure paths tested, not just the happy path.
- Follow every other rule of the project CLAUDE.md files (anonymous logs, `bytes`/`hex`, no game data in a repo).

## The report

At the end, `roadmap/solo-run-report.md` on the branch: what was done (commit by commit), what is provisional, the
questions, what to check in the lab, and what's left of the scope.
