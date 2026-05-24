#!/usr/bin/env bash
#
# Single-command workspace publish for the heic crates.
#
# Reads the current workspace version from Cargo.toml's
# [workspace.package].version, validates everything per CLAUDE.md's
# release rules, and publishes each crate to crates.io in dependency
# order. The version itself is set by the human before running this
# (one edit, then `cargo update`, then commit + push so CI runs).
#
# Flow (each step is a hard gate — failure exits non-zero before any
# irreversible action):
#
#   1. Verify working tree clean + on main bookmark.
#   2. Read workspace version + confirm with user.
#   3. cargo test --workspace --all-targets + --doc.
#   4. cargo audit + cargo deny.
#   5. Verify CI is green on the current HEAD across all platforms.
#   6. cargo publish --dry-run for every crate (catches missing
#      README / metadata / unpublished path deps before the network
#      step).
#   7. If PUBLISH_DRY=true, exit successfully here.
#   8. Tag + push tag + create GitHub release.
#   9. Explicit `PUBLISH` confirmation prompt (per CLAUDE.md —
#      `cargo publish` is irreversible, must require explicit OK).
#  10. cargo publish each crate in topological order with a 30s
#      gap so crates.io has time to index between dependent uploads.
#
# Env vars:
#   PUBLISH_DRY=true      run steps 1–7 then exit
#   PUBLISH_SKIP_CI=true  skip step 5 (use only for emergencies —
#                         CI green is a CLAUDE.md hard requirement)
#   PUBLISH_FORCE=true    skip the interactive confirmation prompt
#                         (do NOT use in interactive shells — meant
#                         for fully-automated release pipelines that
#                         pre-validate everything else)
set -euo pipefail

cd "$(dirname "$0")/.."

# ── Crates in topological publish order ──────────────────────────────────
#
# heic-core has no internal deps; the five backend crates depend on it;
# the parent heic crate (last) depends on heic-core plus every
# backend-* as optional features. Each step inside the loop sleeps
# briefly to let crates.io index the previous upload before the next
# one resolves it as a dependency.
CRATES=(
    heic-core
    heic-backend-mediafoundation
    heic-backend-videotoolbox
    heic-backend-mediacodec
    heic-backend-vaapi
    heic-backend-d3d11va
    heic
)

step()    { printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
warn()    { printf '\033[1;33mWARN:\033[0m %s\n' "$*" >&2; }
fail()    { printf '\033[1;31mFAIL:\033[0m %s\n' "$*" >&2; exit 1; }
require() { command -v "$1" >/dev/null 2>&1 || fail "missing command: $1"; }

require cargo
require gh
require jj
require git
require jq

# ── 1. Working tree clean + on main ──────────────────────────────────────

step "Checking working tree state"
# `self.empty()` is a jj template predicate: true when the current
# change (`@`) has zero diff from its parent. We want it to be
# empty (no uncommitted edits) AND its parent to be the `main`
# bookmark.
EMPTY=$(jj log -r '@' -T 'self.empty()' --no-graph 2>/dev/null)
if [ "$EMPTY" != "true" ]; then
    fail "Working tree (@) has uncommitted changes. Describe + commit first."
fi

# `jj log -r 'main' --no-graph -T 'commit_id'` returns the commit
# at the main bookmark. `@-` is the parent of our current change.
MAIN_SHA=$(jj log -r 'main' --no-graph -T 'commit_id' 2>/dev/null | head -1)
PARENT_SHA=$(jj log -r '@-' --no-graph -T 'commit_id' 2>/dev/null | head -1)
[ -z "$MAIN_SHA" ] && fail "Could not resolve main bookmark"
[ -z "$PARENT_SHA" ] && fail "Could not resolve @- commit"
if [ "$MAIN_SHA" != "$PARENT_SHA" ]; then
    warn "Current change (@) parent ($PARENT_SHA) != main ($MAIN_SHA)."
    warn "Releases ship from main. Rebase onto main@origin first."
    fail "Refusing to release from non-main commit."
fi

# ── 2. Read workspace version ─────────────────────────────────────────────

step "Reading workspace version"
VERSION=$(awk '/^\[workspace\.package\]/{flag=1; next} /^\[/{flag=0} flag && /^version = "/{gsub(/^version = "|"$/, ""); print; exit}' Cargo.toml)
[ -z "$VERSION" ] && fail "Could not extract [workspace.package].version from Cargo.toml"
echo "    Workspace version: $VERSION"
echo "    Crates to publish: ${CRATES[*]}"

# ── 3. Local tests ───────────────────────────────────────────────────────

step "Running local tests (lib + doc + all-targets)"
# Per CLAUDE.md: "REJECT publish requests when tests haven't passed
# locally". --all-targets covers examples + benchmarks; --doc runs
# doctests separately because cargo doesn't combine them.
cargo test --workspace --features "backend-rust,std" --all-targets
cargo test --workspace --features "backend-rust,std" --doc

# ── 4. Supply chain ──────────────────────────────────────────────────────

step "Running cargo audit + cargo deny"
cargo audit --deny warnings --ignore RUSTSEC-2024-0436
cargo deny check

# ── 5. CI green on HEAD ──────────────────────────────────────────────────

if [ "${PUBLISH_SKIP_CI:-false}" != "true" ]; then
    step "Verifying CI is green on the current HEAD"
    SHA=$(git rev-parse HEAD)
    # `gh run list --commit` filters by the commit SHA; pick the most
    # recent CI run for that commit and check its conclusion.
    CI_JSON=$(gh run list --workflow CI --commit "$SHA" --limit 1 --json conclusion,status,databaseId)
    CI_STATUS=$(echo "$CI_JSON" | jq -r '.[0].conclusion // .[0].status // "missing"')
    if [ "$CI_STATUS" != "success" ]; then
        RUN_ID=$(echo "$CI_JSON" | jq -r '.[0].databaseId // "?"')
        fail "CI on $SHA is '$CI_STATUS' (run $RUN_ID). Push, wait for green, re-run."
    fi
    echo "    CI is green on $SHA"
else
    warn "PUBLISH_SKIP_CI=true — skipping CI verification (CLAUDE.md hard requirement)"
fi

# ── 6. Dry-run packaging ─────────────────────────────────────────────────

step "Dry-run cargo publish for each crate"
# --no-verify skips the post-package build step. Without it, the dry-
# run for heic-backend-mediafoundation (which depends on heic-core
# v$VERSION) fails because heic-core v$VERSION isn't on crates.io
# yet — there's no way to verify a multi-crate workspace publish
# end-to-end before the first crate goes up. CI separately verifies
# every crate builds; the dry-run here only validates the
# packaging step (file inclusion, README presence, Cargo.toml
# parseability, license string, etc.).
for crate in "${CRATES[@]}"; do
    echo "    -- $crate"
    cargo publish --dry-run --no-verify -p "$crate" --allow-dirty
done

if [ "${PUBLISH_DRY:-false}" = "true" ]; then
    step "PUBLISH_DRY=true — stopping after dry-run. Set PUBLISH_DRY=false to publish for real."
    exit 0
fi

# ── 7. Tag + GitHub release ──────────────────────────────────────────────

step "Tagging v$VERSION"
if git rev-parse "v$VERSION" >/dev/null 2>&1; then
    warn "Tag v$VERSION already exists locally. Re-using."
else
    git tag "v$VERSION"
    git push origin "v$VERSION"
fi

step "Creating GitHub release v$VERSION"
if gh release view "v$VERSION" >/dev/null 2>&1; then
    warn "GitHub release v$VERSION already exists. Skipping create."
else
    gh release create "v$VERSION" \
        --title "v$VERSION" \
        --generate-notes \
        --target main
fi

# ── 8. Confirmation ──────────────────────────────────────────────────────

if [ "${PUBLISH_FORCE:-false}" != "true" ]; then
    echo
    echo "About to publish ${#CRATES[@]} crates to crates.io at version $VERSION:"
    for crate in "${CRATES[@]}"; do echo "  - $crate v$VERSION"; done
    echo
    echo "This is IRREVERSIBLE. Crates.io does not allow re-publishing"
    echo "the same (name, version) pair, and yanking only hides the version"
    echo "from new resolution — published bytes stay public forever."
    echo
    read -rp "Type 'PUBLISH' to confirm: " confirm
    [ "$confirm" = "PUBLISH" ] || fail "Aborted at confirmation."
else
    warn "PUBLISH_FORCE=true — skipping interactive confirmation"
fi

# ── 9. Publish in topological order ──────────────────────────────────────

for crate in "${CRATES[@]}"; do
    step "Publishing $crate v$VERSION"
    cargo publish -p "$crate"
    # crates.io needs ~10-30s to index a new version before
    # dependent crates can resolve it; sleep here to keep the
    # subsequent publish from failing with "version not found".
    if [ "$crate" != "heic" ]; then
        echo "    (waiting 30s for crates.io to index)"
        sleep 30
    fi
done

step "Published ${#CRATES[@]} crates at v$VERSION."
echo
echo "Verify on:"
for crate in "${CRATES[@]}"; do
    echo "  https://crates.io/crates/$crate/$VERSION"
done
