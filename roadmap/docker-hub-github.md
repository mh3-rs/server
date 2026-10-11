# Moving to Docker Hub and GitHub

**Status: draft (not started; the user, 2026-10-11)**

Goal: anyone can build mh3sp from public sources, and run it from an image on Docker Hub. Today everything lives on
the self-hosted Gitea (`git.pband.ch/experiment`, private) and its Woodpecker (`ci.pband.ch`).

## What is private today

- **The repositories**: `core` (mh3sp-rs), `shared`, `tunnel`, `brain`, `cli`, `website`, `docs`, all private.
- **Our crates**: `shared` and `tunnel` are published to the organization's Gitea cargo registry, private: `core`
  (`server`, `testkit`), `cli` and `brain` pin them exactly (`=0.1.0`, `registry = "gitea"`, `.cargo/config.toml`).
  Cargo needs a token for them, even to build the server without bots: it fetches every source of the lockfile.
- **The image**: Woodpecker builds it (`.woodpecker/image.yaml`) into `git.pband.ch/experiment/mh3sp-rs`, tagged
  `main`, the commit, and the server's version (`X`, `X.Y`, `X.Y.Z`). The `Dockerfile` reads the Gitea token as a
  build secret, for the crates.
- **CI**: Woodpecker, with the global secrets `gitea_user` and `gitea_token`.

## What has to move, in order

1. **The crates, fetchable without a token.**
   - (a) Git dependencies on the public repositories, pinned to a tag (`tunnel = { git = "…", tag = "0.1.0" }`): no
     registry account, nothing published for good.
   - (b) crates.io (`mh3sp-shared`, `mh3sp-tunnel`, names to check): the usual way, but a version published there
     stays forever (a yank only hides it).
   - (c) Vendored into `core` as workspace members: no second repository, but `cli` and `brain` then depend on `core`.

   Recommended: (a). The `Dockerfile`'s token secret and `.cargo/config.toml`'s registry go with it.
2. **The sources on GitHub**, public (AGPL-3.0): Gitea pushes to them (a push mirror per repository), so the work
   stays where it is. The alternative, GitHub as the origin and Gitea dropped, moves the CI too.
3. **Docker Hub**: an account and a repository (`<account>/mh3sp`), a token as Woodpecker secrets
   (`docker_hub_user`, `docker_hub_token`). `image.yaml` pushes the same tags there; the buildx plugin can log in to
   several registries (`logins`, to check against `woodpeckerci/plugin-docker-buildx:5.0.0`, the only version the
   server trusts).
4. **The README and `deploy/compose.yaml`**: the image's name becomes Docker Hub's, the `docker login` line goes.

## Open

- GitHub: a mirror (recommended) or the origin?
- The crates: git tags (recommended), crates.io, or vendored?
- Docker Hub's account and repository name.
- Pull requests on GitHub: with a mirror, they get no CI (Woodpecker watches Gitea). Accepted as long as
  contributions come in rarely, or GitHub Actions for a check only.
- When: before 1.0.0 (the roadmap's last step puts the image on Docker Hub), after the first crash test.
