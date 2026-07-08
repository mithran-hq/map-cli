# Forge CLI Operator Guide

This guide maps the Forge-facing MAP CLI surface to the local source,
commands, control-plane routes, auth requirements, and verification evidence in
`map-cli`.

Central Forge documentation lives in `mithran-business`:

- [Forge documentation hub](https://github.com/mithran-hq/mithran-business/blob/main/docs/forge/README.md)
- [Forge architecture](https://github.com/mithran-hq/mithran-business/blob/main/docs/forge/architecture.md)
- [Forge agent/operator guide](https://github.com/mithran-hq/mithran-business/blob/main/docs/forge/agent-operator-guide.md)
- [Forge documentation plan](https://github.com/mithran-hq/mithran-business/blob/main/docs/forge/documentation-plan.md)

## Operating Boundary

`map-cli` owns the thin public command-line client for MAP operations:

- Saving and reading local MAP login state.
- Printing a saved token for the Jason controller audience when the login state
  allows that audience.
- Validating deploy target refs and SHAs before requests leave the operator
  machine.
- Calling MAP control-plane deploy, onboard, access, route, publish, evidence,
  rollback, status, and readiness endpoints.
- Scaffolding `mithran.yaml` and, only when explicitly requested, the BYO-CI
  GitHub Actions deploy workflow.
- Packaging the `map` binary as an Aegis host component artifact.

The repo does not own hosted control-plane behavior, deploy review gates,
build orchestration, source broker policy, sidecar admission, Auth token
issuance, runtime-control worker provisioning, route programming, package
assembly in Aegis.app, or Jason scheduling. Those are server or consumer repo
concerns. Operators should use this repo to understand the local CLI contract
and the HTTP calls it makes.

## Source Map

CLI entrypoint and command model:

- `src/main.rs` defines the `map` binary, global flags, subcommands, command
  arguments, and all command handlers.
- `Cli` defines global `--login-state`, `--endpoint`, `--token`, and `--json`.
- `Command` defines `login`, `whoami`, `doctor`, `init`, `validate`,
  `deploy`/`deploy-request`, `onboard`, `access`, `versions`,
  `publish`, `canary`, `status`, `watch`, `logs`, `evidence`, `rollback`, and
  `version`.

Auth and local state:

- `LoginState` stores `map_control_endpoint`, optional
  `jason_controller_endpoint`, `access_token`, audience, scopes, expiry, and
  principal details.
- `login_state_path` reads `--login-state`, `MITHRAN_LOGIN_STATE`,
  `AEGIS_LOGIN_STATE`, or defaults to `$XDG_CONFIG_HOME/mithran/login.json`
  with `$HOME/.config/mithran/login.json` as the home fallback.
- `map login save` writes local JSON state.
- `map login print-token --audience <audience>` prints the saved token only
  when the state audience matches, has `map:*`, or carries
  `audience:<audience>`.
- `--endpoint` plus `--token` bypasses local state and creates an in-memory
  `map-control` login for that command.

Deploy and status flow:

- `deploy_request` validates `--ref` or `--sha`, normalizes a bare
  `owner/repo` to `github://owner/repo`, then POSTs
  `/v1/map-control/deploy/request`.
- `status` GETs `/v1/map-control/deploy/status` with `deployment_ref`.
- `watch` polls `/v1/map-control/deploy/status` until the status is terminal.
  `Succeeded` is success; `Failed`, `Superseded`, `RolledBack`,
  `ReviewBlocked`, `BuildFailed`, `RuntimeFailed`, and `RouteFailed` fail the
  watch.
- `logs` intentionally returns a CLI error because the live control plane does
  not expose a deploy logs route.
- `evidence` GETs `/v1/map-control/deploy/evidence`.
- `rollback` POSTs `/v1/map-control/deploy/rollback`.

Onboarding and BYO-CI:

- `onboard` validates `owner/repo`, POSTs `/v1/map-control/onboard`, then
  writes `mithran.yaml` into `--repo-dir` when the file is absent.
- The webhook-native path is the default. `onboard` writes no workflow unless
  the operator passes `--with-ci-workflow`.
- `scaffold_deploy_workflow` writes `templates/map-deploy.yml` under
  `<repo-dir>/.github/workflows/<workflow>` for the explicit BYO-CI path.
- `set_repo_variables` writes non-secret `MAP_*` GitHub Actions Variables for
  BYO-CI when a GitHub token is available. It reports failures in JSON and does
  not fail onboarding.

Access, publish, and canary:

- `resolve_access_policy` reads `access.yaml` or `--file`, rejects unknown
  fields, merges CLI overrides, defaults declared policy to `protected`, and
  prepares the `/v1/map-control/access` body.
- `access plan` prints the resolved policy without a control-plane call.
- `access apply` POSTs `/v1/map-control/access`.
- `versions` GETs `/v1/map-control/routes/status`, filters pointers for the
  requested app, and separates addressable internal versions, aliases, and the
  published external pointer. Active canary aliases include the canary
  deployment ref and weight in both text and JSON output.
- `publish` resolves `--version` through `routes/status` or accepts
  `--deployment-ref`, then POSTs `/v1/map-control/deploy/publish` with optional
  `--expected-sha` and `--actor`.
- `canary start` validates `--weight` locally as 1..99, normalizes `<app>` to
  `app:<app>`, then POSTs `/v1/map-control/deploy/canary` with `canary_action:
  start`, `canary_deployment_ref`, and `weight_pct`.
- `canary promote` and `canary rollback` POST the same endpoint with
  `canary_action: promote` or `rollback` and the named `canary_deployment_ref`.
  Text output reports the action, app, deployment ref, alias/hostname when
  returned, and result; `--json` prints the server response unchanged.

Packaging and release:

- `scripts/package_component.py` builds `cargo build --release` and emits the
  generic `aegis.component.v1` component artifact.
- `scripts/package_host_component.py` packages a supplied macOS `map` binary as
  `aegis.map_cli.host_component.v1`, including binary hash, source ref, target
  arch, signing state, and version probe evidence.
- `.github/workflows/ci.yml` runs `cargo fmt --check`, `cargo test`, and
  `python3 scripts/package_component.py target/map-cli-component.tar.gz`.
- `.github/workflows/component-artifacts.yml` builds the arm64 macOS host
  component on `main` and publishes durable release assets.

## Control-Plane Routes

`map-cli` calls these routes through the saved or explicit MAP control endpoint:

| Command | Method and route |
| --- | --- |
| `map doctor` | `GET /v1/map-control/config`, `GET /v1/map-control/routes/status` |
| `map deploy`, `map deploy-request` | `POST /v1/map-control/deploy/request` |
| `map onboard` | `POST /v1/map-control/onboard` |
| `map access apply` | `POST /v1/map-control/access` |
| `map versions` | `GET /v1/map-control/routes/status` |
| `map publish` | `POST /v1/map-control/deploy/publish` |
| `map canary start`, `map canary promote`, `map canary rollback` | `POST /v1/map-control/deploy/canary` |
| `map status`, `map watch` | `GET /v1/map-control/deploy/status` |
| `map evidence` | `GET /v1/map-control/deploy/evidence` |
| `map rollback` | `POST /v1/map-control/deploy/rollback` |

The BYO-CI template also calls:

- `GET $ACTIONS_ID_TOKEN_REQUEST_URL&audience=$MAP_OIDC_AUDIENCE` to obtain a
  GitHub OIDC token from Actions.
- `POST /v1/auth/github-oidc/exchange` on `MAP_AUTH_ENDPOINT` to mint a short
  lived MAP control token.
- `POST /v1/map-control/deploy/request` on `MAP_CONTROL_ENDPOINT`.

## Operator Commands

Save a MAP control login:

```bash
map login save \
  --map-control-endpoint https://control-plane.sandbox.mithran.cloud \
  --access-token "$MITHRAN_TOKEN" \
  --scope map:* \
  --scope audience:jason-controller
```

Use explicit endpoint and token for one command:

```bash
map --endpoint https://control-plane.sandbox.mithran.cloud \
  --token "$MITHRAN_TOKEN" doctor
```

Onboard a repository for the webhook-native path:

```bash
map onboard mithran-hq/demo \
  --installation-ref github-installation://131136661 \
  --repo-dir ./demo
```

Opt into the BYO-CI workflow:

```bash
map onboard mithran-hq/demo \
  --installation-ref github-installation://131136661 \
  --repo-dir ./demo \
  --with-ci-workflow
```

Trigger a direct deploy request:

```bash
map deploy \
  --repo mithran-hq/demo \
  --env staging \
  --ref refs/heads/release/1.2 \
  --installation-ref github-installation://131136661 \
  --app-ref app:demo
```

Inspect and publish an internal version:

```bash
map versions demo
map publish demo --version demo-2 --expected-sha <40-hex-sha>
```

Start, promote, or rollback a Forge canary:

```bash
map canary start demo \
  --deployment-ref deployment://sandbox/production/demo-3 \
  --weight 20
map canary promote demo \
  --deployment-ref deployment://sandbox/production/demo-3
map canary rollback demo \
  --deployment-ref deployment://sandbox/production/demo-3
```

Plan and apply app access:

```bash
map access plan --file access.yaml
map access apply --file access.yaml
```

## Local Evidence Commands

Primary local gate:

```bash
cargo fmt --check
cargo test
python3 scripts/package_component.py target/map-cli-component.tar.gz
```

Focused evidence:

```bash
cargo run -- version
cargo run -- validate --repo mithran-hq/demo --sha 0123456789abcdef0123456789abcdef01234567
cargo run -- login save \
  --login-state /tmp/map-login.json \
  --map-control-endpoint https://control-plane.sandbox.mithran.cloud \
  --access-token test-token \
  --scope map:*
python3 scripts/package_host_component.py \
  --binary target/release/map \
  --target-arch aarch64 \
  --version local \
  --source-ref git:local \
  --output-dir dist/map-cli
```

Evidence locations:

- `templates/map-deploy.yml` is the BYO-CI workflow template.
- `target/map-cli-component.tar.gz` is the generic package smoke output.
- `dist/map-cli/` is the host component output directory.
- `mithran.yaml` is the scaffolded app manifest written into a tenant repo.
- `access.yaml` is the app access policy input file read by `map access`.

This repo has no local ADR or runbook tree. The operative Forge and MAP
architecture references are the central Forge docs linked above plus the root
`README.md`.

## Symptom Triage

| Symptom | Inspect |
| --- | --- |
| `map` cannot read login state | Check `--login-state`, `MITHRAN_LOGIN_STATE`, `AEGIS_LOGIN_STATE`, `$XDG_CONFIG_HOME/mithran/login.json`, and `$HOME/.config/mithran/login.json`. Run `map login save` or pass `--endpoint` and `--token`. |
| `print-token` rejects the requested audience | Check login state `audience` and `scopes`. The state must match the requested audience, contain `map:*`, or contain `audience:<audience>`. |
| Deploy target validation fails | Pass exactly one usable source selector: `--ref <git-ref>` or `--sha <40-hex-sha>`. |
| Deploy request is rejected by source policy | Check `--repo`, `--installation-ref`, `--app-ref`, tenant/account refs, and the control-plane source registry binding created by `map onboard`. |
| `map onboard` returns conflict with an install URL | Install or grant the GitHub App at the returned URL, then rerun `map onboard`; the command is designed to be idempotent. |
| `map onboard --with-ci-workflow` reports variable failures | Check `GITHUB_TOKEN`, `GH_TOKEN`, or `gh auth token`, plus repo permissions for Actions Variables. The variables are non-secret `MAP_*` values. |
| BYO-CI workflow cannot mint a token | Check `id-token: write`, `MAP_AUTH_ENDPOINT`, `MAP_OIDC_AUDIENCE`, and whether the repository has an active onboarding binding. |
| `map doctor` fails config or route checks | Check the saved endpoint, token, `/v1/map-control/config`, `/v1/map-control/routes/status`, and whether the app has an allowlist or registry binding. |
| `map logs` fails | This is expected: the live control plane exposes no deploy logs route. Use `map status` or `map evidence`. |
| `map versions` shows no published version | The app has addressable internal versions but no external published pointer. Run `map publish` after a version has passed server review. |
| `map publish` returns 409 | The version is stale or not publishable. Re-run `map versions`, verify the reviewed succeeded deployment, and pass the current `--expected-sha` if using the stale-safe guard. |
| `map canary start` rejects `--weight` | Use an integer from 1 through 99. Use `promote` for 100% and `rollback` to clear the split. |
| `map canary` returns canary target errors | Re-run `map versions` or `map status` and verify the named deployment exists and is succeeded/promoted before starting or ending a split. |
| Packaging fails | Run `cargo build --release` first for `scripts/package_component.py`; for `scripts/package_host_component.py`, pass a built macOS `map` binary and supported `--target-arch aarch64`. |

## Review Checklist

Before closing a Forge CLI docs or behavior task:

- The docs index links this guide.
- This guide links the central Forge docs.
- CLI-owned behavior is separated from control-plane, Auth, sidecar,
  runtime-control, Aegis package assembly, and Jason authority.
- Route, command, source path, and packaging claims match local source.
- At least one local verification command has been run and recorded.
- Changed docs do not create untracked required work.
