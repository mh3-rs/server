#!/usr/bin/env sh
# Releases `dev` into `prod` (or a hotfix already on `prod`): runs scripts/ci.sh,
# merges with --no-ff, bumps the version in Cargo.toml / package.json, prepends
# the release to CHANGELOG.md (git-cliff, cliff.toml), commits it all as the
# version (`1.2.3`), tags it, and fast-forwards `dev`. Never pushes.
#
# The version comes from the Conventional Commits on `dev` (cliff.toml's [bump]),
# except 1.0.0: going to 1.0.0 is a decision, so it is never automatic.
#   scripts/release.sh            the next version, from the commits
#   scripts/release.sh 1.0.0      that version
#   scripts/release.sh --dry-run  show the version and the changelog only
#
# Copied into each project: adapt the bump step if versions live elsewhere
# (sibling crates' dependency lines, a nested package.json, openapi.json...).
set -eu
cd "$(dirname "$0")/.."

die() {
    echo "$1" >&2
    exit 1
}
command -v git-cliff >/dev/null 2>&1 || die "git-cliff is missing (brew install git-cliff)"
dry=
next=
for arg in "$@"; do
    case "$arg" in
    --dry-run) dry=1 ;;
    *) next=$arg ;;
    esac
done

branch=$(git branch --show-current)
if [ -z "$dry" ]; then # a dry run only reads: any branch, any tree
    case "$branch" in
    dev | prod) ;; # prod: a hotfix squashed into prod
    *) die "releases start from dev (or prod, for a hotfix)" ;;
    esac
    [ -z "$(git status --porcelain)" ] || die "commit or stash your changes first"
fi
git remote | grep -qx origin && git fetch -q --tags origin

# Another session may have pushed since: a branch that is only behind origin's is
# fast-forwarded. One that also has commits of its own stops here, for a hand to rebase
# them: a rebase done blindly would drop an unpushed release's version commit.
sync() {
    git rev-parse -q --verify "refs/remotes/origin/$1" >/dev/null || return 0
    [ "$(git rev-list --count "$1..origin/$1")" != 0 ] || return 0
    [ "$(git rev-list --count "origin/$1..$1")" = 0 ] ||
        die "$1 and origin/$1 have diverged: git pull --rebase on $1 (push an unpushed release first), then release again"
    if [ "$1" = "$branch" ]; then
        git merge -q --ff-only "origin/$1"
    else
        git fetch -q . "origin/$1:$1"
    fi
}
if [ -z "$dry" ]; then
    git rev-parse -q --verify refs/heads/dev >/dev/null && sync dev
    git rev-parse -q --verify refs/heads/prod >/dev/null && sync prod
fi

if [ -f Cargo.toml ]; then
    current=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
elif [ -f package.json ]; then
    current=$(node -p "require('./package.json').version") # any indentation
else
    die "no Cargo.toml or package.json: adapt this script"
fi

if [ -z "$next" ]; then
    if git tag --list '[0-9]*.*.*' | grep -q .; then
        next=$(git cliff --bumped-version)
        [ "$next" != 1.0.0 ] || die "1.0.0 is your call: scripts/release.sh 1.0.0"
    else
        next=$current # the first release: the version the manifest already has
    fi
fi
echo "$next" | grep -Eqx '[0-9]+\.[0-9]+\.[0-9]+' || die "not a version: $next"
git rev-parse -q --verify "refs/tags/$next" >/dev/null && die "$next is already released"

# The changelog section, written now: dev's commits aren't on prod until the merge.
changes=$(mktemp)
git cliff --unreleased --tag "$next" --strip header >"$changes"

if [ -n "$dry" ]; then
    echo "Next version: $next (now $current)"
    cat "$changes"
    exit 0
fi

scripts/ci.sh || die "scripts/ci.sh failed: nothing released"

if [ "$branch" = dev ]; then
    git checkout -q prod
    git merge -q --no-ff --no-commit dev >/dev/null || die "merge conflict: resolve it, or git merge --abort"
fi

if [ -f Cargo.toml ]; then
    sed -i.bak "s/^version = \"$current\"$/version = \"$next\"/" Cargo.toml
    rm Cargo.toml.bak
    cargo update -q --workspace
fi
if [ -f package.json ]; then
    npm version "$next" --no-git-tag-version --allow-same-version >/dev/null
fi
if [ -s "$changes" ]; then # empty when only skipped commits (chore, test...)
    [ -f CHANGELOG.md ] || printf '# Changelog\n\n' >CHANGELOG.md
    { head -n 2 CHANGELOG.md; cat "$changes"; echo; tail -n +3 CHANGELOG.md; } >"$changes.new"
    mv "$changes.new" CHANGELOG.md
    git add CHANGELOG.md
fi
rm -f "$changes"

git add -u
git commit -q -m "$next"
git tag -a "$next" -m "$next"
git checkout -q dev
if [ "$branch" = dev ]; then
    git merge -q --ff-only prod
    echo "Released $next. Push: git push origin prod dev $next"
else
    echo "Released $next on prod. Bring it into dev: git merge prod, then push prod, dev and $next"
fi
