# MAP CLI

`map` is the thin command-line client for MAP 1.0. It is distributed by
`Aegis.pkg` and talks to the hosted `mithran-control-plane` service over its
`/v1/map-control/deploy/*` routes.

The client does not build, schedule, or run applications locally. Deployments
are always based on committed GitHub refs or SHAs.

## Quick Start

```sh
map login save \
  --map-control-endpoint https://map.example.com \
  --access-token "$MITHRAN_TOKEN" \
  --scope map:* \
  --scope audience:jason-controller

# Onboard an app: record the source-registry binding + scaffold mithran.yaml.
# GitHub App webhooks are the default deploy path; no repo workflow is written.
map onboard mithran-hq/demo --installation-ref github-installation://131136661 --repo-dir ./demo

# Optional: add a custom-CI deploy workflow for repos that intentionally trigger
# MAP deploys from GitHub Actions.
map onboard mithran-hq/demo --installation-ref github-installation://131136661 \
  --repo-dir ./demo --with-ci-workflow

# Diagnose readiness against the saved control-plane endpoint.
map doctor --app mithran-hq/demo

# Review mithran.yaml locally before deploy.
map deploy-review --repo-root ./demo

# Trigger a direct deploy request. Standard GitHub refs should usually deploy
# through the GitHub App webhook path after onboarding.
map deploy --repo mithran-hq/demo --env production --ref refs/heads/release/1.2 \
  --installation-ref github-installation://131136661 --app-ref app:demo

# List an app's addressable internal versions and current published version.
map versions demo

# Publish a reviewed, succeeded internal version to the app's clean public URL.
map publish demo --version demo-2

# Start, promote, or rollback a Forge canary on the production alias.
map canary start demo --deployment-ref "$DEPLOYMENT_REF" --weight 20
map canary promote demo --deployment-ref "$DEPLOYMENT_REF"
```

Jason can reuse the MAP login by asking for a controller token:

```sh
map login print-token --audience jason-controller
```

## Deploy Model

The GitHub App installation and webhook are the default deploy trigger. After a
repository is onboarded, a `git push` to a matching ref is verified and
forwarded to the control-plane deploy request route. The default path does not
write a workflow file into the application repository and does not require a
per-repo deploy secret.

- `map deploy` / `map deploy-request` POST **directly** to the control-plane
  `/v1/map-control/deploy/request` using your saved `map-control` login token (the same call;
  `deploy-request` is the explicit host/runner-side spelling). It uses the configured
  authenticated control-plane endpoint. No GitHub Actions workflow is dispatched.
- `map onboard <owner/repo> --installation-ref <ref>` records the source-registry binding and
  scaffolds a starter `mithran.yaml`. It writes **no** repo workflow by default.
- `map deploy-review [--repo-root .] [--manifest mithran.yaml]` reviews the app manifest locally
  before deploy. It uses the public `map-deploy-review-contract` crate for the
  `map.mithran/v1` contract and emits hard blocking `ERR_*` findings. It does not upload
  source, create deployment state, mutate routes, or mint evidence.
- **Opt-in custom CI:** pass `--with-ci-workflow` to `map onboard` to also scaffold
  the keyless-OIDC `.github/workflows/map-deploy.yml` (`curl` → OIDC token exchange →
  `/deploy/request`). This is for repos that intentionally trigger deploys from GitHub Actions;
  it is not needed for the default webhook path. The workflow reads required production repo
  Variables and fails clearly if they are absent.

## Publish Model

A deploy makes an internal version *addressable* (its own per-version URL); it does **not**
move the app's clean, env-bare public URL. That public URL is a separate **published**
pointer the operator advances explicitly, so developers can iterate on internal versions
without changing what end-users see.

- `map versions <app>` reads `/v1/map-control/routes/status` and lists the app's addressable
  internal versions (label → `deployment_ref` → per-version hostname), its aliases
  (production/preview/release), and which internal version is currently published (or
  `(not published)`). When the production alias has an active canary split, alias output also
  shows the canary deployment ref and weight. `<app>` is the app name, normalized to `app:<app>`.
- `map publish <app> [--version <label> | --deployment-ref <ref>] [--expected-sha <sha>]
  [--actor <ref>]` POSTs `/v1/map-control/deploy/publish` to pin the public URL to a chosen
  version. `--version` is resolved to a `deployment_ref` via `routes/status`. The
  control-plane is **review-gated** (rejects unless the version is a reviewed, succeeded
  deploy) and **stale-safe** (with `--expected-sha`, rejects if the version's recorded source
  SHA moved). On success it prints the published URL.

## Canary Model

Canary operations mutate the app's production alias through the control-plane canary endpoint:

- `map canary start <app> --deployment-ref <ref> --weight <1-99>` validates the weight locally,
  then POSTs `/v1/map-control/deploy/canary` with action `start`, the canary deployment ref, and
  `weight_pct`. The control-plane requires the target deployment to be succeeded or promoted.
- `map canary promote <app> --deployment-ref <ref>` POSTs the same endpoint with action
  `promote`, moving the active canary to current at 100% and clearing the split.
- `map canary rollback <app> --deployment-ref <ref>` POSTs action `rollback`, clearing the split
  and keeping current production at 100%.
- Text output reports the action, app, canary deployment ref, alias/hostname when returned, and
  result. `--json` prints the server response unchanged.

`map domain` (custom-domain binding) is a separate capability and is not part of this CLI.

## Boundary

Public:

- local auth-state discovery;
- MAP deploy/status/log/evidence client commands;
- token handoff for the Jason hosted client.

Private:

- `mithran-control-plane`;
- build orchestration;
- sidecar admission;
- runtime-control and worker provisioning.
