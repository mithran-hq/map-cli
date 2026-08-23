---
title: MAP CLI repository shape
answers: What does the MAP CLI own, and where are its source, packaging, and local test surfaces?
last_verified: 2026-08-23
verified_ref: 6e1e2dc37428eba4d99960e47e5d72b96fa4c601
refresh: quarterly
---

# Repository shape

The MAP CLI is a thin client. It sends authenticated commands to the MAP control plane.

```claim
id: repository.boundary
kind: ASSERTED
statement: The README describes map as a thin MAP 1.0 client that talks to the hosted mithran-control-plane service.
source: README.md
quote: "map is the thin command-line client for MAP 1.0. It is distributed by Aegis.pkg and talks to the hosted mithran-control-plane service."
verified_at: 2026-08-23
verified_ref: 6e1e2dc37428eba4d99960e47e5d72b96fa4c601
```

```claim
id: repository.source-and-fixtures
kind: MEASURED
statement: The repository tracks one Rust source entry point, one deploy template, and one CLI hygiene test.
command: git ls-files src/main.rs templates/map-deploy.yml tests/cli_token_hygiene.rs | sort | paste -sd' ' -
expect: "src/main.rs templates/map-deploy.yml tests/cli_token_hygiene.rs"
match: exact
verified_at: 2026-08-23
verified_ref: 6e1e2dc37428eba4d99960e47e5d72b96fa4c601
```

```claim
id: repository.control-plane-routes
kind: MEASURED
statement: The Rust client contains the direct MAP deploy request route.
command: grep -F -l '/v1/map-control/deploy/request' src/main.rs
expect: "src/main.rs"
match: exact
verified_at: 2026-08-23
verified_ref: 6e1e2dc37428eba4d99960e47e5d72b96fa4c601
```

```claim
id: repository.review-contract-dependency
kind: MEASURED
statement: The CLI source references the public deploy-review contract dependency.
command: grep -F -l 'map-deploy-review-contract' Cargo.toml src/main.rs
expect: |
  Cargo.toml
  src/main.rs
match: exact
verified_at: 2026-08-23
verified_ref: 6e1e2dc37428eba4d99960e47e5d72b96fa4c601
```

```claim
id: repository.local-review-surface
kind: MEASURED
statement: The repository tracks a deploy-review fixture matrix for local manifest review.
command: git ls-files tests/fixtures/map-deploy-review-contract/cases.yml
expect: "tests/fixtures/map-deploy-review-contract/cases.yml"
match: exact
verified_at: 2026-08-23
verified_ref: 6e1e2dc37428eba4d99960e47e5d72b96fa4c601
```

```claim
id: repository.release-scripts
kind: MEASURED
statement: The repository tracks Python scripts for component packaging and publication.
command: git ls-files 'scripts/*.py' | sort | paste -sd' ' -
expect: "scripts/package_component.py scripts/package_host_component.py scripts/publish_component_release.py scripts/test_publish_component_release.py"
match: exact
verified_at: 2026-08-23
verified_ref: 6e1e2dc37428eba4d99960e47e5d72b96fa4c601
```
