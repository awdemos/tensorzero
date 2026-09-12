# VibeGate Publish-Surface Rebrand — Design

**Date:** 2026-09-12
**Status:** Approved
**Repo:** `awdemos/tensorzero` fork (local: `/var/home/a/code/tensorzero`)
**Downstream consumer:** `vibecodingagency` (local: `/var/home/a/code/vibecodingagency`, jj-managed)

## Background

TensorZero (the upstream company) shut down in June 2026; its trademark, GitHub org,
Docker Hub org, and domain are controlled by parties other than this fork. The fork is
Apache-2.0 licensed, so the code remains free to use, modify, and distribute — but
publishing artifacts under the `tensorzero` name carries trademark exposure.

Decision: rename the project's **publish surface** to **VibeGate**, a fully distinct
name (verified free on Docker Hub and npm, 2026-09-12) that carries the vibecodingagency
brand. Internal technical identifiers stay unchanged so upstream merges and the live
deployment remain unaffected.

## Goals

- Publish fork-built container images under the VibeGate name.
- Zero changes to functional interfaces (config format, env vars, API, crate names).
- Attribution and license hygiene per Apache-2.0.
- Downstream deploy (vibecodingagency) consumes the new images.

## Non-goals

- Renaming Rust crates, TS package names, env vars, or config keys.
- Renaming internal CI/dev image references (e.g. `tensorzero/gateway-dev:sha-*`).
- Gateway `--version` string, UI chrome, or other end-user-visible branding.
- Publishing language packages (npm/crates.io).

## What Changes

### 1. Image publishing

New namespace and coordinates:

| Component  | GHCR (primary, immediate)                    | Docker Hub (secondary, after org creation) |
|------------|----------------------------------------------|--------------------------------------------|
| gateway    | `ghcr.io/awdemos/vibegate/gateway`           | `vibegate/gateway`                         |
| ui         | `ghcr.io/awdemos/vibegate/ui`                | `vibegate/ui`                              |
| evaluations| `ghcr.io/awdemos/vibegate/evaluations`       | `vibegate/evaluations`                     |

- Tag scheme: calendar versioning continuing upstream lineage (`2026.6.x`), plus `latest`
  on release, plus `sha-<sha>` dev tags if needed.
- Multi-arch: `linux/amd64` + `linux/arm64`, matching upstream's build matrix.
- GHCR works immediately with the built-in `GITHUB_TOKEN` (`packages: write`).
- Docker Hub requires the user to create the `vibegate` org and set
  `DOCKERHUB_USERNAME` / `DOCKERHUB_TOKEN` repository secrets; wiring lands in the same
  workflow behind an `if: secrets-available` style guard so it no-ops until configured.

### 2. Fork CI

Add one new workflow, `.github/workflows/vibegate-publish.yml`:

- Trigger: `release: [released]` and `workflow_dispatch`.
- Guard: `github.repository == 'awdemos/tensorzero'` (mirrors upstream's self-guard
  pattern, keeps it inert elsewhere).
- Matrix: upstream's platform matrix (ubuntu-24.04 / ubuntu-24.04-arm) × the three
  containers (gateway: `crates/gateway/Dockerfile`, ui, evaluations), built with
  `PROFILE=performance` for releases (upstream publishes release images from the
  non-dev profile; the `-dev` variant only applies to the `workflow_call` dev path,
  which the fork does not need initially).
- Steps: checkout → set up buildx → log in to GHCR (`GITHUB_TOKEN`) → build-and-push
  with both arch manifests under one tag. Docker Hub login/push added later as a
  guarded second registry once secrets exist.
- `permissions: packages: write, contents: read, attestations: write, id-token: write`
  (same as upstream).
- Upstream's `docker-hub-publish.yml` remains untouched: its existing
  `if: github.repository == 'tensorzero/tensorzero'` guard keeps it from ever running
  on the fork.

### 3. Branding and attribution

- README: title/branding becomes VibeGate, with a header statement: VibeGate is an
  independent community fork of TensorZero, distributed under the Apache-2.0 license,
  not affiliated with or endorsed by the former TensorZero company.
- `LICENSE` file: unchanged (Apache-2.0 requires preserving copyright notices).
- Source copyright headers: untouched.

### 4. Downstream consumers (vibecodingagency repo, jj-managed)

- `fly/tensorzero/Dockerfile`: `FROM tensorzero/gateway:latest` →
  `FROM ghcr.io/awdemos/vibegate/gateway:latest`.
- `fly/tensorzero/README.md`: update the wrap reference (rename of that directory
  itself is optional and out of scope).
- `ci/dagger/main.go`: default tag constant `tensorzero/gateway:latest` →
  `ghcr.io/awdemos/vibegate/gateway:latest`.
- Changes follow that repo's jj conventions (proposed via `jj new`, not raw git mutation).

## What Stays Identical

- Crate names (`gateway`, `tensorzero-core`, ...), binary name `gateway`, TS packages
  (`@tensorzero/tensorzero-node` — workspace-internal only).
- Env vars (`TENSORZERO_POSTGRES_URL`, provider key env vars), config file name and
  format (`tensorzero.toml`), API routes.
- Gateway `--version` output, UI chrome.
- Internal CI/dev image names (`tensorzero/gateway-dev:sha-*`, e2e images) — not
  user-facing, revisited later only if desired.

## Rollout

1. Land this spec + the new workflow on `awdemos/tensorzero` `main`.
2. Manual-dispatch the workflow to publish `2026.6.1` (or next version) to GHCR.
3. Verify multi-arch manifests exist (`docker manifest inspect`).
4. Build the vibecodingagency fly wrapper against the new GHCR image; smoke-run locally
   (`--version`, boot with config, `/health`).
5. Propose/land the downstream `FROM` + dagger tag changes; redeploy
   `ultrawork-tensorzero` via the existing dagger/fly path.
6. (Follow-up, user-driven) Create the `vibegate` Docker Hub org, add secrets, extend
   the workflow to push Docker Hub.

## Verification

- `docker manifest inspect ghcr.io/awdemos/vibegate/gateway:2026.6.1` shows amd64+arm64.
- Fly wrapper image boots: `gateway --version` → `gateway 2026.6.1`, `/health` 200 with
  the production-shaped config.
- Production: `flyctl status` checks passing, `/health` 200 after redeploy.
- No references to `ghcr.io/awdemos/vibegate` appear anywhere except the fork's
  workflow/README and the downstream consumer files.

## Risks

- GHCR images under `awdemos` org visibility depends on org package settings; set
  packages to public for the vibegate repo (forks of public repos usually inherit
  public visibility, but verify at first publish).
- Docker Hub `vibegate` org creation is a manual user step; the workflow must no-op
  cleanly until secrets exist.
- Pinning: downstream `fly/tensorzero/Dockerfile` should eventually pin a version tag
  instead of `latest`; noted here but kept as-is in this pass to minimize diff.
