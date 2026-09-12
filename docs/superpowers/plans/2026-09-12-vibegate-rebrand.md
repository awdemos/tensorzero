# VibeGate Publish-Surface Rebrand Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish the fork's gateway/ui/evaluations images under the VibeGate name (GHCR first), rebrand the fork README, and repoint the vibecodingagency deploy at the new images — with zero functional/interface renames.

**Architecture:** New guarded GitHub Actions workflow mirrors upstream's `docker-hub-publish.yml` two-phase build (per-arch digest push on native runners + manifest merge) but targets `ghcr.io/awdemos/vibegate/*` with the built-in `GITHUB_TOKEN`. README gets a VibeGate header with Apache-2.0 fork attribution. Downstream `fly/tensorzero` wrapper and fork `dagger/main.go` default tag repoint to the GHCR coordinate.

**Tech Stack:** GitHub Actions (docker/build-push-action, buildx imagetools), GHCR, Docker (rootless Podman locally — always `docker build --network host`), flyctl, jj (vibecodingagency repo).

**Spec:** `docs/superpowers/specs/2026-09-12-vibegate-rebrand-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `.github/workflows/vibegate-publish.yml` | Create | Release/dispatch publish of gateway, ui, evaluations images to GHCR, multi-arch |
| `README.md` | Modify | VibeGate header + fork attribution block (lines 1–8) |
| `dagger/main.go` | Modify | Default image tag constant → GHCR VibeGate coordinate (lines 23, 43) |
| `vibecodingagency/fly/tensorzero/Dockerfile` | Modify (jj repo) | `FROM` → GHCR VibeGate image |
| `vibecodingagency/fly/tensorzero/README.md` | Modify (jj repo) | Update "wraps the official tensorzero/gateway image" line |

Unchanged deliberately: all crate names, `TENSORZERO_*` env vars, `tensorzero.toml` config name/format, `.github/workflows/docker-hub-publish.yml` (self-guarded to `tensorzero/tensorzero`), source copyright headers, `LICENSE`.

---

### Task 1: Add the VibeGate publish workflow

**Files:**
- Create: `.github/workflows/vibegate-publish.yml`

- [ ] **Step 1: Write the workflow**

```yaml
# Publishes VibeGate container images (gateway, ui, evaluations) to GHCR.
# Structure mirrors upstream docker-hub-publish.yml, retargeted to
# ghcr.io/awdemos/vibegate/* with the built-in GITHUB_TOKEN.
# Docker Hub (vibegate/*) lands later behind a secrets guard once the org exists.

name: Publish VibeGate images

permissions: {}

on:
  workflow_dispatch:
    inputs:
      version:
        description: "Version tag to publish (e.g. 2026.6.1)"
        type: string
        required: true
        default: "2026.6.1"
  release:
    types: [released]

concurrency:
  group: vibegate-publish-${{ github.ref || github.run_id }}
  cancel-in-progress: false

jobs:
  build:
    name: Build and push image (${{ matrix.container.name }}, ${{ matrix.platform.target }})
    runs-on: ${{ matrix.platform.runner }}
    timeout-minutes: 90
    if: github.repository == 'awdemos/tensorzero'
    strategy:
      fail-fast: false
      matrix:
        platform:
          - runner: ubuntu-24.04
            target: linux/amd64
          - runner: ubuntu-24.04-arm
            target: linux/arm64
        container:
          - name: gateway
            path: crates/gateway
          - name: ui
            path: ui
          - name: evaluations
            path: crates/evaluations
    permissions:
      packages: write
      contents: read
      attestations: write
      id-token: write
    steps:
      - name: Check out the repo
        uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd # v6.0.2
        with:
          persist-credentials: false

      - name: Prepare
        env:
          PLATFORM_TARGET: ${{ matrix.platform.target }}
        run: |
          platform="$PLATFORM_TARGET"
          echo "PLATFORM_PAIR=${platform//\//-}" >> $GITHUB_ENV

      - name: Login to GHCR
        uses: docker/login-action@74a5d142397b4f367a81961eba4e8cd7edddf772 # v3.4.0
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}

      - name: Set up Docker Buildx
        uses: docker/setup-buildx-action@8d2750c68a42422c14e847fe6c8ac0403b4cbd6f # v3.12.0

      - name: Extract metadata (tags, labels) for Docker
        id: meta
        uses: docker/metadata-action@c299e40c65443455700f0fdfc63efafe5b349051 # v5.10.0
        with:
          images: ghcr.io/awdemos/vibegate/${{ matrix.container.name }}

      - name: Build and push Docker image
        id: push
        uses: docker/build-push-action@10e90e3645eae34f1e60eeb005ba3a3d33f178e8 # v6.19.2
        with:
          platforms: ${{ matrix.platform.target }}
          file: ./${{ matrix.container.path }}/Dockerfile
          # Make '.git' available in the build context:
          # https://github.com/docker/build-push-action/issues/513#issuecomment-987951050
          context: .
          push: true
          provenance: mode=max
          outputs: type=image,push-by-digest=true,name-canonical=true,push=true
          tags: ghcr.io/awdemos/vibegate/${{ matrix.container.name }}
          labels: ${{ steps.meta.outputs.labels }}
          sbom: true
          no-cache: ${{ github.event_name == 'release' }}
          build-args: |
            PROFILE=performance
            DEBUG_BUILD=0

      - name: Export digest
        env:
          PUSH_DIGEST: ${{ steps.push.outputs.digest }}
        run: |
          mkdir -p "$RUNNER_TEMP/digests"
          digest="$PUSH_DIGEST"
          touch "$RUNNER_TEMP/digests/${digest#sha256:}"

      - name: Upload digest
        uses: actions/upload-artifact@330a01c490aca151604b8cf639adc76d48f6c5d4 # v5.0.0
        with:
          name: digests-${{ matrix.container.name }}-${{ env.PLATFORM_PAIR }}
          path: ${{ runner.temp }}/digests/*
          if-no-files-found: error
          retention-days: 1

  merge:
    name: Merge multi-arch manifests (${{ matrix.container.name }})
    runs-on: ubuntu-latest
    timeout-minutes: 30
    needs:
      - build
    if: github.repository == 'awdemos/tensorzero'
    strategy:
      matrix:
        container:
          - name: gateway
          - name: ui
          - name: evaluations
    permissions:
      packages: write
      contents: read
    steps:
      - name: Download digests
        uses: actions/download-artifact@018cc2cf5baa6db3ef3c5f8a56943fffe632ef53 # v6.0.0
        with:
          path: ${{ runner.temp }}/digests
          pattern: digests-${{ matrix.container.name }}-*
          merge-multiple: true

      - name: Login to GHCR
        uses: docker/login-action@74a5d142397b4f367a81961eba4e8cd7edddf772 # v3.4.0
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}

      - name: Set up Docker Buildx
        uses: docker/setup-buildx-action@8d2750c68a42422c14e847fe6c8ac0403b4cbd6f # v3.12.0

      - name: Docker meta
        id: meta
        uses: docker/metadata-action@c299e40c65443455700f0fdfc63efafe5b349051 # v5.10.0
        with:
          images: ghcr.io/awdemos/vibegate/${{ matrix.container.name }}
          flavor: |
            latest=false
          tags: |
            type=ref,event=tag
            type=raw,value=${{ inputs.version }},enable=${{ github.event_name == 'workflow_dispatch' }}
            type=sha,format=long
            type=raw,value=latest,enable=${{ github.event_name == 'release' && github.event.prerelease == false }}

      - name: Create manifest list and push
        working-directory: ${{ runner.temp }}/digests
        run: |
          docker buildx imagetools create $(jq -cr '.tags | map("-t " + .) | join(" ")' <<< "$DOCKER_METADATA_OUTPUT_JSON") \
            $(printf "ghcr.io/awdemos/vibegate/${MATRIX_CONTAINER_NAME}@sha256:%s " *)
        env:
          MATRIX_CONTAINER_NAME: ${{ matrix.container.name }}

      - name: Wait for image to be available on GHCR
        run: |
          IMAGE="ghcr.io/awdemos/vibegate/${MATRIX_CONTAINER_NAME}:${STEPS_META_OUTPUTS_VERSION}"
          echo "Waiting for ${IMAGE} to be available on GHCR..."
          for i in $(seq 1 30); do
            if docker manifest inspect "${IMAGE}" > /dev/null 2>&1; then
              echo "Image ${IMAGE} is available"
              exit 0
            fi
            echo "Attempt ${i}/30: Image not yet available, waiting 10s..."
            sleep 10
          done
          echo "Timed out waiting for ${IMAGE}"
          exit 1
        env:
          MATRIX_CONTAINER_NAME: ${{ matrix.container.name }}
          STEPS_META_OUTPUTS_VERSION: ${{ steps.meta.outputs.version }}
```

- [ ] **Step 2: Validate the YAML parses**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/vibegate-publish.yml'))"`
Expected: exits 0, no output.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/vibegate-publish.yml
git commit -m "ci: add VibeGate GHCR publish workflow"
```

---

### Task 2: README VibeGate header and attribution

**Files:**
- Modify: `README.md:1-8`

- [ ] **Step 1: Replace the top block**

Old (exact):

```html
<p><picture><img src="https://github.com/user-attachments/assets/9d0a93c6-7685-4e57-9737-7cbeb338218" alt="TensorZero Logo" width="128" height="128"></picture></p>

# TensorZero

> [!IMPORTANT]
> This fork of TensorZero is under new management and is now developed independently of the upstream TensorZero project, which has discontinued development. This repository will continue to evolve on its own roadmap. The original docs, links, and branding below are historical and may not reflect this fork's current status.
```

New (exact):

```html
<p><picture><img src="https://github.com/user-attachments/assets/9d0a93c6-7685-4e57-9737-7cbeb338218" alt="VibeGate Logo" width="128" height="128"></picture></p>

# VibeGate

> [!IMPORTANT]
> VibeGate is an independent community fork of [TensorZero](https://github.com/tensorzero/tensorzero), distributed under the Apache-2.0 license. It is not affiliated with or endorsed by the former TensorZero company. The upstream project discontinued development in June 2026; VibeGate continues maintenance and development on its own roadmap. The original docs, links, and some branding below are historical and may not reflect this fork's current status. Container images are published as `ghcr.io/awdemos/vibegate/*`.
```

Note: the logo image URL is kept as-is (it is GitHub user-attachment hosting on this very repo's README history; swapping artwork is out of scope). Everything below the block stays untouched.

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: rebrand README header as VibeGate with fork attribution"
```

---

### Task 3: Fork dagger module default tag

**Files:**
- Modify: `dagger/main.go:23` and `dagger/main.go:43`

- [ ] **Step 1: Update the comment and default**

Old line 23: `// Container image tag (default "tensorzero/gateway:latest")`
New line 23: `// Container image tag (default "ghcr.io/awdemos/vibegate/gateway:latest")`

Old line 43: `tag = "tensorzero/gateway:latest"`
New line 43: `tag = "ghcr.io/awdemos/vibegate/gateway:latest"`

- [ ] **Step 2: Verify it still builds**

Run: `cd dagger && go build ./...`
Expected: exits 0, no output. (The generated `dagger/internal` package already exists in the working tree; if `go` is missing, run `gofmt -l main.go` instead and expect no output.)

- [ ] **Step 3: Commit**

```bash
git add dagger/main.go
git commit -m "chore: point dagger default image tag at VibeGate GHCR coordinate"
```

Do NOT commit `dagger/dagger.gen.go` or `dagger/internal/` — pre-existing generated files, deliberately untracked.

---

### Task 4: Push fork and publish 2026.6.1 to GHCR

**Files:** none (remote operations)

- [ ] **Step 1: Push main**

```bash
git push origin main
```
Expected: `main -> main` fast-forward.

- [ ] **Step 2: Confirm Actions can run on the fork**

Visit `https://github.com/awdemos/tensorzero/actions`. If it shows "Actions are disabled" or an enablement banner, click **Enable workflows** (required for manual dispatch). If the VibeGate workflow does not appear under "Actions" yet, that is normal until the first push containing it clears; verify it appears after Step 1.

- [ ] **Step 3: Dispatch the workflow**

Option A (web): Actions → "Publish VibeGate images" → Run workflow → version `2026.6.1` → Run.
Option B (CLI, if `gh` is authenticated with repo scope): `gh workflow run vibegate-publish.yml --repo awdemos/tensorzero -f version=2026.6.1`

- [ ] **Step 4: Wait for the run to finish**

Watch the run in the Actions tab. Expected: 6 build jobs + 3 merge jobs, all green, in roughly 20–40 minutes (gateway compile dominates; native arch runners avoid emulation).

- [ ] **Step 5: Verify multi-arch manifests**

```bash
echo "$(gh auth token)" | docker login ghcr.io -u awdemos --password-stdin
docker manifest inspect ghcr.io/awdemos/vibegate/gateway:2026.6.1 | grep -c 'architecture'
```
Expected: `2` (amd64 + arm64). Repeat for `ui:2026.6.1` and `evaluations:2026.6.1`.
(If `gh` is unavailable, authenticate to ghcr.io at https://github.com/settings/tokens with `read:packages` and use the token directly.)

- [ ] **Step 6: Make the packages public**

In `https://github.com/orgs/awdemos/packages`, open each `vibegate/*` package → Package settings → Change visibility → Public. (Needed so unauthenticated deploy hosts — e.g. Fly's builders — can pull without registry auth.)

---

### Task 5: Downstream vibecodingagency repoint (jj repo)

**Files:**
- Modify: `/var/home/a/code/vibecodingagency/fly/tensorzero/Dockerfile:5`
- Modify: `/var/home/a/code/vibecodingagency/fly/tensorzero/README.md:26`

- [ ] **Step 1: Create a jj change on top of main**

```bash
cd /var/home/a/code/vibecodingagency
jj new main -m "chore: point fly gateway wrapper at VibeGate GHCR image"
```

- [ ] **Step 2: Update the Dockerfile FROM line**

Old: `FROM tensorzero/gateway:latest`
New: `FROM ghcr.io/awdemos/vibegate/gateway:latest`

- [ ] **Step 3: Update the README line**

Old: `` - `Dockerfile` wraps the official `tensorzero/gateway` image. ``
New: `` - `Dockerfile` wraps the VibeGate gateway image (`ghcr.io/awdemos/vibegate/gateway`). ``

(Exact surrounding text is in `fly/tensorzero/README.md`; keep the rest of the line/file intact.)

- [ ] **Step 4: Verify with jj and push**

```bash
jj status
jj diff
```
Expected: exactly two files modified, matching Steps 2–3. Then:

```bash
jj git push
```
Expected: pushes the new change to the git remote.

---

### Task 6: Smoke-build the wrapper against the published image

**Files:** none (local verification)

- [ ] **Step 1: Build the fly wrapper locally**

```bash
docker build --network host -t tz-wrapper-smoke:vibegate /var/home/a/code/vibecodingagency/fly/tensorzero
```
Expected: build succeeds; base layer pulls from GHCR (requires Step 4/6 auth from Task 4, or public package visibility).

- [ ] **Step 2: Verify the binary**

```bash
docker run --rm --network host --entrypoint gateway tz-wrapper-smoke:vibegate --version
```
Expected: `gateway 2026.6.1`.

- [ ] **Step 3: Clean up the smoke image**

```bash
docker rmi tz-wrapper-smoke:vibegate
```

---

### Task 7: Production deploy of ultrawork-tensorzero from the VibeGate image

**Files:** none (deploy, replicates the sequence already proven on 2026-09-12)

- [ ] **Step 1: Build and push the deploy wrapper**

```bash
TS=$(date -u +%Y%m%d-%H%M%S)
TAG="registry.fly.io/ultrawork-tensorzero:deploy-$TS"
docker build --network host -t "$TAG" /var/home/a/code/vibecodingagency/fly/tensorzero
docker push "$TAG"
```
Expected: push succeeds with a digest.

- [ ] **Step 2: Deploy**

```bash
cd /var/home/a/code/vibecodingagency/fly/tensorzero
flyctl deploy --app ultrawork-tensorzero --image "$TAG" --yes
```
Expected: rolling update, both machines reach a good state, health checks pass.

- [ ] **Step 3: Verify production**

```bash
curl -s -o /dev/null -w '%{http_code}\n' https://ultrawork-tensorzero.fly.dev/health
flyctl status -a ultrawork-tensorzero
```
Expected: `200`; the app row shows the new deploy image with checks passing.

- [ ] **Step 4: Record the rollback ref**

Note the previous release image tag shown in `flyctl status` before this deploy (format `deployment-XXXXXXXXXXXXXXXXXXXX`), and keep it as the rollback reference in case a rollback is needed later.

---

### Task 8: Docker Hub org creation (manual, user-owned)

**Files:** none

- [ ] **Step 1: Create the org**

Create account/org `vibegate` at https://hub.docker.com (user action; cannot be automated here).

- [ ] **Step 2: Add repo secrets**

In `awdemos/tensorzero` → Settings → Secrets and variables → Actions: add `DOCKERHUB_USERNAME` and `DOCKERHUB_TOKEN` (token needs write access to the `vibegate` org).

- [ ] **Step 3: Extend the workflow (future pass, not in this plan)**

Add a guarded Docker Hub login + second imagetools push to `vibegate-publish.yml` once secrets exist. Defer until org is created; the GHCR images are the production source of truth in the meantime.

---

## Self-Review Notes

- **Spec coverage:** image publishing (Task 1, 4), branding/attribution (Task 2), fork dagger tag (Task 3), downstream repoint (Task 5), verification/rollout (Tasks 4, 6, 7), Docker Hub follow-up (Task 8). All spec sections mapped.
- **Placeholder scan:** all file contents, commands, and expected outputs are concrete; the only deferred item (Docker Hub push) is an explicit non-goal with a dedicated placeholder task.
- **Consistency:** image coordinate `ghcr.io/awdemos/vibegate/<name>` used identically in workflow, README, dagger constant, Dockerfile, and commands; tag `2026.6.1` matches the binary version published in the release.
