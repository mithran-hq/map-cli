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
