# Releasing the heic workspace

The workspace ships **seven crates** under a single synchronized
version. One bump per release; one command publishes them all.

## Workspace crates (publish order)

| # | Crate | Purpose |
|---|---|---|
| 1 | [`heic-core`](heic-core/) | Shared types (`HevcBackend` trait, `DecodedFrame`, color conversion, SPS/PPS). |
| 2 | [`heic-backend-mediafoundation`](heic-backend-mediafoundation/) | Windows MediaFoundation backend. |
| 3 | [`heic-backend-videotoolbox`](heic-backend-videotoolbox/) | Apple VideoToolbox backend (macOS / iOS / tvOS / visionOS). |
| 4 | [`heic-backend-mediacodec`](heic-backend-mediacodec/) | Android NDK AMediaCodec backend. |
| 5 | [`heic-backend-vaapi`](heic-backend-vaapi/) | Linux libva (VA-API) backend. |
| 6 | [`heic-backend-d3d11va`](heic-backend-d3d11va/) | Windows D3D11VA backend. |
| 7 | [`heic`](.) | Parent crate — HEIF container + dispatcher + allowlist API. |

The order matters: each crate depends only on those listed above it
(plus the optional native bindings). `scripts/release.sh` enforces
this order and sleeps between uploads so crates.io can index a new
version before the next dependent crate references it.

## Version scheme

All seven crates share **one version**, set in
`[workspace.package].version` in the root `Cargo.toml`. Bump it
together — there is no "advance heic-core but not heic" path because
the parent's `heic-core = "X.Y.Z"` dependency must always match.

Workspace dependency version specifiers in the root `[workspace.
dependencies]` table also need to bump to the same number so the
parent + sibling crates resolve correctly when the path attribute
gets stripped at publish time. Both fields are tracked together
under the version-bump commit.

## Single-command publish

### Recommended flow

```bash
# 1. Bump the version in Cargo.toml (both [workspace.package].version
#    and the heic-core / heic-backend-* entries under
#    [workspace.dependencies]). Run `cargo update` so the lockfile
#    catches up.
$EDITOR Cargo.toml
cargo update

# 2. Update CHANGELOG.md: move [Unreleased] entries into a new
#    [X.Y.Z] - YYYY-MM-DD section.
$EDITOR CHANGELOG.md

# 3. Commit + push. CI must go green BEFORE step 4 — `just publish`
#    refuses to release against a non-green commit per CLAUDE.md.
jj commit -m "release: vX.Y.Z"
jj git push

# 4. Verify the packaging without touching crates.io.
just publish-dry

# 5. Ship it. Asks for explicit "PUBLISH" confirmation before the
#    irreversible upload step.
just publish
```

### What `just publish` does

The script enforces every guardrail in CLAUDE.md's release section:

1. **Working tree clean + on main** — refuses to release from a
   side branch or with uncommitted edits.
2. **`cargo test --workspace --all-targets`** + **`cargo test --doc`**
   — full local suite must pass.
3. **`cargo audit --deny warnings`** + **`cargo deny check`** —
   supply-chain gates.
4. **CI green on HEAD** — `gh run list --commit HEAD` must show
   `success`. No "I'll fix CI after publishing"; the bytes are
   immutable once on crates.io.
5. **`cargo publish --dry-run -p <crate>` for every crate** —
   catches missing READMEs, broken doc-links, unpublished path
   deps.
6. **`git tag v<VERSION>` + `git push origin v<VERSION>`** —
   tag-then-release pairing required.
7. **`gh release create v<VERSION>` with auto-generated notes** —
   CLAUDE.md mandates a GitHub release page per published version,
   not just a tag.
8. **Explicit "PUBLISH" confirmation prompt** — last gate before
   the irreversible step.
9. **`cargo publish -p <crate>` in topological order** with a 30 s
   crates.io indexing pause between dependent crates.

### Env-var knobs

```bash
PUBLISH_DRY=true   ./scripts/release.sh   # alias for `just publish-dry`
PUBLISH_SKIP_CI=true ./scripts/release.sh # SKIP step 4 — emergencies only
PUBLISH_FORCE=true ./scripts/release.sh   # skip the interactive prompt
                                          # — only for automated pipelines
                                          #   that already gate manually
```

CLAUDE.md hard rules: **`PUBLISH_SKIP_CI=true` should never be used
on the standard release path.** It exists for the case where the GH
Actions provider has degraded service and a critical security patch
must ship despite. Use a fresh signed commit + a manually-verified
local CI run if you have to.

## Bumping for breaking changes

Breaking-change releases (any 0.x → 0.(x+1) bump per Cargo semver
rules) need:

1. A `### QUEUED BREAKING CHANGES` section already in
   `[Unreleased]` listing every break.
2. `cargo semver-checks` run against the previous published version
   confirming the breaking list is complete.
3. The CHANGELOG entry for the new version documents each break +
   the migration path.

Patch bumps (0.x.y → 0.x.(y+1)) require:

1. `cargo semver-checks` reports **zero** breaking changes.
2. Bug-fix entries only in the CHANGELOG release section.

## Yank policy

**Never yank a published version** except for:

- Confirmed security vulnerability in our code (not a transitive
  dep — those are tracked via `cargo audit` advisories).
- Semver-breaking change shipped accidentally as a patch.

Yanking only hides the version from future resolution; the bytes
remain public. The right fix is almost always **ship a new version**
with the issue resolved.

## First-time setup

The crate dispatch infrastructure shipped in 0.2.0; six of the
seven crates are publishing to crates.io for the first time with
that release. The remaining crate (`heic`) was previously at 0.1.6
and bumps to 0.2.0 with this same release to mark the breaking
backend split.
