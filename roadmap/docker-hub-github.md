# Public mirrors: GitHub and Docker Hub

**Status: agreed (2026-10-14)**

Goal: anyone can read and build the server from public sources and run it from a public Docker Hub image, while the
work stays where it is. Gitea (`git.pband.ch/mh3-rs`) is the origin and Woodpecker (`ci.pband.ch`) the CI;
GitHub and Docker Hub are read-only mirrors that the CI feeds from `main` only, and only with what fully passed.
Lasting rule: `../docs/architecture.md` (CI).

## What is mirrored

- **GitHub:** `core`, `shared`, `tunnel` (core doesn't build without them). Nothing else: `brain`, `cli`, `website`
  and `docs` stay private.
- **Docker Hub:** the server's image, public, no login to pull.
- Names: GitHub [`mh3-rs/server`](https://github.com/mh3-rs/server) (core),
  [`mh3-rs/shared`](https://github.com/mh3-rs/shared), [`mh3-rs/tunnel`](https://github.com/mh3-rs/tunnel); Docker Hub
  [`mh3rs/server`](https://hub.docker.com/r/mh3rs/server) (created by the first push). The repository is named after
  what it is; `mh3sp-rs` under `mh3-rs` would say "rs" twice.
- **Gitea, done first** (the user, 2026-10-14): the project left the `experiment` organization for its own,
  `mh3-rs`, the same name as on GitHub; core's repository became `server`, the others lost their `mh3sp-` prefix
  (`git.pband.ch/mh3-rs/{server,docs,brain,cli,website,shared,tunnel}`). Gitea redirects the old names; its packages
  stay with their old owner, so the image under `mh3-rs/server` starts with this sprint's first `main` build, and the
  deployment (`~/git/perso/infrastructure/always-up/mh3.yml`) switches to `git.pband.ch/mh3-rs/server:latest` once
  that image exists.

## The crates: git dependencies on GitHub

Every user of `shared` and `tunnel` (core's `server` and `testkit`, `cli`, `brain`) pins them as a git dependency on
the GitHub mirror, at a commit, with its version checked:
`shared = { git = "https://github.com/mh3-rs/shared", rev = "<sha>", version = "=0.1.0" }`. Cargo refuses the build if
the commit's manifest says another version, so both ends of the API and of the tunnel still agree.

A new crate version: bump `version` in the crate, commit to `main`; once its mirror has pushed it, update `rev` and
`version` in each user, `cargo update -w`, CI, commit.

Goes away: the `gitea` registry (`.cargo/config.toml` in core, `cli`, `brain`), `publish.yaml` in `shared` and
`tunnel`, the token in every `ci.yaml`, the Dockerfile's build secret.

## The mirror step

One Woodpecker pipeline per mirrored repository (`.woodpecker/mirror.yaml`): on a push to `main` only, once every
other pipeline passed (core: the checks and the image; `shared`, `tunnel`: the checks), it force-pushes
`HEAD:refs/heads/main` to GitHub over SSH with the repository's own deploy key (secret `github_deploy_key`, one per
repository, set by the user). Needs a full clone (`clone: depth: 0`). No tags.

Core's image pipeline skips a docs-only push (`*.md`, `roadmap/`): how Woodpecker treats a skipped dependency is
checked while implementing; at worst such a push reaches GitHub with the next image build (accepted: rebuilding the
image for a README fix costs a full amd64 + arm64 build).

## The image

`image.yaml`: one build for `linux/amd64,linux/arm64`, pushed to both registries (`repo` as a list, `logins` for
two registries; checked in the plugin's v5.0.0 source) with one tag list: `latest`, the commit (8 characters, for a
rollback), `X`, `X.Y`, `X.Y.Z` (`crates/server/Cargo.toml`). New secrets `docker_hub_user`, `docker_hub_token`.
The deployment pulls `:latest` from Gitea (Docker Hub's
pull quotas).

## README and `deploy/compose.yaml`

The Docker Hub image, no `docker login`; the README says the GitHub repositories are mirrors (issues disabled, where
to reach us instead). `docs/CLAUDE.md`'s "Our crates" and `00. global.md`'s CI paragraph say the new way.

## Decisions

1. **Only core and its two crates** go public: they are what builds and runs the server. The website, the brain and
   the CLI aren't needed for that; rejected: mirroring all six now.
2. **Gitea stays the origin, GitHub is a mirror**: the work, the CI and the deployment stay where they are. Rejected:
   GitHub as the origin (moves the CI).
3. **The CI pushes, not Gitea's push mirror**: it pushes only `main` and only after everything passed. Gitea's mirror
   pushes every branch, gated on nothing.
4. **Force-push**: Gitea is the one truth; a rewritten `main` still syncs.
5. **No tags this sprint**, anywhere: they come later, from the CI, after the checks and the images.
6. **Crates pinned by `rev` + `version`**, not by tag (no tags yet) nor crates.io (a published version stays for
   good), nor vendored into core (`cli` and `brain` would depend on core). One source of truth: GitHub, for every user,
   private ones included; the Gitea registry goes.
7. **One deploy key per GitHub repository**: GitHub refuses a key on two, and each key can write one repository only.
   Rejected: one token for all (wider blast radius).
8. **One tag list for both registries**: the plugin takes one list for every `repo`. Rejected: a copy step with
   `crane` (one more tool), two builds (arm64 under emulation twice). Gitea loses its `main` tag; the commit tag stays
   for rollbacks.
9. **arm64 back** on every `main` build: a public image is pulled on ARM hosts. Cost: slower builds (emulation).
10. **Public names in plain text** in the YAML (GitHub, Docker Hub): they are public anyway, and a secret would mask
    them in the logs. Woodpecker's built-in variables where they exist.
11. **The corpus goes public as is**: lab captures, private addresses only; the tests need it. The author email is
    public too (the user's call).
12. **Issues disabled on GitHub**; pull requests can't be, the README points elsewhere.

## Tests

No code changes the server's behaviour: the proof is the CI and a clean build.

- `scripts/ci-tests.sh` (core) and `scripts/ci.sh` (`shared`, `tunnel`, `cli`, `brain`) green, without any registry
  token in the environment.
- The pipelines checked with `woodpecker-cli exec` where it runs locally, then after the push with
  `woodpecker-cli pipeline ps`.
- "A failing `main` reaches nothing" is read off the `depends_on` chain, not tried with a broken commit.

## Order of the work

1. `shared`, `tunnel`: the mirror pipeline, `publish.yaml` goes; push, check GitHub has `main`.
2. core: git dependencies, registry and token out, `image.yaml` (arm64, Docker Hub, the tags), the mirror pipeline.
   `cli`, `brain`: git dependencies, registry out.
3. README, `deploy/compose.yaml`, `docs/CLAUDE.md`, `00. global.md`, `../docs/architecture.md`.

## Not in this sprint

The website (mirror, image, licence), `brain` and `cli` on GitHub, tags on GitHub and Docker Hub, CI for pull
requests on GitHub, the Docker Hub page's description.

## Done when

- A machine without any token clones core from GitHub and builds it (`cargo build --locked`).
- `docker pull mh3rs/server:latest` runs on amd64 and on arm64 (the Mac).
- A push to another branch reaches neither GitHub nor Docker Hub; a failing `main` reaches nothing (the
  `depends_on` chain); a green `main` reaches both.
- `scripts/ci-tests.sh` and CI are green in core, `shared`, `tunnel`, `cli`, `brain`.
