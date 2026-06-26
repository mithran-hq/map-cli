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

map doctor
map deploy --repo mithran-hq/demo --sha 0123456789abcdef0123456789abcdef01234567 --env staging
```

Jason can reuse the MAP login by asking for a controller token:

```sh
map login print-token --audience jason-controller
```

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
