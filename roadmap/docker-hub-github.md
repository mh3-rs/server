# Public mirrors: GitHub and Docker Hub

**Status: draft (big lines, 2026-10-14; to settle in `/sprint-start`)**

Goal: anyone can read and build mh3sp from public sources and run it from a Docker Hub image, while the work stays
where it is: Gitea (`git.pband.ch/experiment`) is the origin, Woodpecker (`ci.pband.ch`) the CI, and the deployment
pulls from the Gitea registry (Docker Hub's quotas). GitHub and Docker Hub are read-only mirrors.

## Settled (the user, 2026-10-14)

- **Mirrored to GitHub:** `core`, `shared`, `tunnel` (core doesn't build without them), `brain`, `cli`, `website`.
  **Not mirrored:** `docs` (stays private), `content`, `lab`, `reference` (upstream MH3SP).
- **Only `main` and the version tags** reach GitHub: feature and experiment branches never leave Gitea.
- **Gitea stays the origin**; nothing is developed on GitHub.
- **The old dev TLS key** (`certs/dev/server.key`, in core's history from `8ab4ae5`, removed in `dda43d0`): left in
  the history. It was a dev-only CA, replaced by a CA per deployment; no rewrite.
- Secrets checked (gitleaks on every repository's whole history, 2026-10-14): nothing else. CI reads credentials
  only through `from_secret`; the Dockerfile's token is a build secret, in no layer. Links to `pband.ch` stay.

## Work, in order

1. **Our crates without a token.** `shared` and `tunnel` become git dependencies on their GitHub mirrors, pinned to
   a tag (`0.1.0` exists). Goes with them: the `gitea` registry in `.cargo/config.toml`, the token in every
   `ci.yaml`, the Dockerfile's build secret, `publish.yaml` in `shared` and `tunnel`; lockfiles regenerated.
   Ordering trap: the GitHub repositories of `shared` and `tunnel` must exist, with their tag, before core, `cli`
   and `brain` switch.
2. **The mirror step**, one per mirrored repository: on a push to `main` or a version tag, Woodpecker pushes
   `HEAD:main` and the tags to GitHub with a deploy key (a secret). Needs a full clone (`clone: depth: 0`).
3. **Docker Hub**: `image.yaml` also pushes to Docker Hub, **on version tags only** (Gitea keeps `main`, the commit
   and the versions for the deployment); `logins` for two registries, new secrets `docker_hub_user` /
   `docker_hub_token`. Release images bring arm64 back.
4. **Licences**: `website` has none (the others are AGPL-3.0).
5. **The README and `deploy/compose.yaml`**: the Docker Hub image, no `docker login git.pband.ch`, a link to
   `https://mh3.pband.ch`; the README says the GitHub repositories are mirrors (issues and contributions: where?).

## Open

- The GitHub owner and repository names; the Docker Hub account and repository (the user gives the links).
- The mirror: a Woodpecker step (recommended: branch control) or Gitea's push mirror, if this Gitea can filter
  branches (unchecked; it pushes every branch otherwise).
- Whether `woodpeckerci/plugin-docker-buildx:5.0.0` (the only trusted version) has `logins`: to check in its docs.
- The deploy key: one per GitHub repository (GitHub refuses a key on two) or one machine user's token for all.
- Docker Hub on version tags means nothing is published there before the first tag: tag a `0.x` now, or wait for
  1.0.0?
- The corpus (13 MB of lab captures: lab consoles' support codes and hunter names, private addresses only, quest
  binaries as MH3SP served them) goes public with core: kept as is (the tests need it)?
- Issues and pull requests on GitHub: disabled, or accepted and carried over to Gitea by hand (no CI there)?
- The commit author email (`pascal@pband.ch`) becomes public: fine?

## Done when

- A clean machine without any token clones core from GitHub and builds it (`cargo build --locked`), and
  `docker pull <docker hub image>` runs.
- A push to a feature branch reaches nothing; a push to `main` reaches GitHub; a version tag reaches GitHub and
  Docker Hub.
- `scripts/ci-tests.sh` and CI are green in every changed repository.
