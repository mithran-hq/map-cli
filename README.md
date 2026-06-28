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

# Onboard an app: drop the deploy workflow + print the host onboarding steps.
map setup mithran-hq/demo --repo-dir ./demo

# Diagnose readiness against the saved control-plane endpoint.
map doctor --app mithran-hq/demo

# Trigger a deploy (ADR-0016): dispatch the thin GitHub Actions workflow.
map deploy --env staging --ref refs/heads/main --repo mithran-hq/demo

# List an app's addressable internal versions + which one is published (ADR-0018).
map versions demo

# Publish a reviewed, succeeded internal version to the app's clean public URL.
map publish demo --version demo-2
```

Jason can reuse the MAP login by asking for a controller token:

```sh
map login print-token --audience jason-controller
```

## Deploy model (ADR-0016)

`map deploy [--env --ref --repo]` does **not** call the control-plane directly. The
control-plane listens on `127.0.0.1:4260` on the host and is not reachable from
GitHub-hosted runners. `map deploy` instead **dispatches** a thin
`.github/workflows/map-deploy.yml` workflow (`workflow_dispatch`, via the GitHub API using
your `gh`/GitHub token); that workflow — running on a self-hosted runner on the host (or a
public ingress) — POSTs the deploy request to `/v1/map-control/deploy/request`. GitHub is
the trigger + audit surface; the deploy-review gate stays server-side.

- `map setup <owner/repo>` adds that workflow to a repo and prints the host onboarding steps
  (allowlist + bare-mirror create + `map-mirror-sync`). The host steps are printed because
  there is no control-plane `onboard` endpoint yet (see map-cli#5).
- `map deploy-request` is the host/runner-side primitive that POSTs straight to the
  control-plane — only usable where `:4260` is reachable (host-local or via a tunnel).

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
