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
# Webhook-native — no repo workflow is written (add --with-ci-workflow for BYO-CI).
map onboard mithran-hq/demo --installation-ref github-installation://131136661 --repo-dir ./demo

# Diagnose readiness against the saved control-plane endpoint.
map doctor --app mithran-hq/demo

# Trigger a deploy (ADR-0016, webhook-native): direct brokered call to the control-plane.
map deploy --repo mithran-hq/demo --env staging --ref refs/heads/release/1.2 \
  --installation-ref github-installation://131136661 --app-ref app:demo

# List an app's addressable internal versions + which one is published (ADR-0018).
map versions demo

# Publish a reviewed, succeeded internal version to the app's clean public URL.
map publish demo --version demo-2
```

Jason can reuse the MAP login by asking for a controller token:

```sh
map login print-token --audience jason-controller
```

## Deploy model (ADR-0016, webhook-native amendment 2026-07-06 — mithran-business#582)

SCM integration is **webhook-native** (the Vercel model). The per-env GitHub App
installation + webhook is the primary deploy trigger: a `git push` to a matching ref is
HMAC-verified by the sidecar and forwarded to the control-plane
`/v1/map-control/deploy/request` — **no workflow file lives in the tenant repo**, and there
is no per-repo deploy secret. The deploy-review gate stays server-side (ADR-0014); GitHub is
a trigger + audit surface, never the gate.

- `map deploy` / `map deploy-request` POST **directly** to the control-plane
  `/v1/map-control/deploy/request` using your saved `map-control` login token (the same call;
  `deploy-request` is the explicit host/runner-side spelling). Reachable wherever the
  control-plane endpoint is — the public authenticated edge, or host-local `:4260` / a tunnel.
  No GitHub Actions workflow is dispatched.
- `map onboard <owner/repo> --installation-ref <ref>` records the source-registry binding and
  scaffolds a starter `mithran.yaml`. It writes **no** repo workflow by default.
- **Opt-in BYO-CI (ADR-0023):** pass `--with-ci-workflow` to `map onboard` to also scaffold
  the keyless-OIDC `.github/workflows/map-deploy.yml` (`curl` → OIDC token exchange →
  `/deploy/request`) and set the `MAP_*` repo Variables it reads. This is for tenants who want
  to trigger deploys from their own CI; it is not needed for the default webhook path.

## Publish model (ADR-0018)

A deploy makes an internal version *addressable* (its own per-version URL); it does **not**
move the app's clean, env-bare public URL. That public URL is a separate **published**
pointer the operator advances explicitly, so developers can iterate on internal versions
without changing what end-users see.

- `map versions <app>` reads `/v1/map-control/routes/status` and lists the app's addressable
  internal versions (label → `deployment_ref` → per-version hostname), its aliases
  (production/preview/release), and which internal version is currently published (or
  `(not published)`). `<app>` is the app name, normalized to `app:<app>`.
- `map publish <app> [--version <label> | --deployment-ref <ref>] [--expected-sha <sha>]
  [--actor <ref>]` POSTs `/v1/map-control/deploy/publish` to pin the public URL to a chosen
  version. `--version` is resolved to a `deployment_ref` via `routes/status`. The
  control-plane is **review-gated** (rejects unless the version is a reviewed, succeeded
  deploy) and **stale-safe** (with `--expected-sha`, rejects if the version's recorded source
  SHA moved). On success it prints the published URL.

`map domain` (custom-domain binding) is a separate, later slice and is not part of this CLI yet.

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
