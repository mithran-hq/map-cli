---
title: MAP CLI deployment boundary hazard
answers: Which deployment responsibilities remain outside the MAP CLI?
last_verified: 2026-08-23
verified_ref: 6e1e2dc37428eba4d99960e47e5d72b96fa4c601
refresh: quarterly
---

# Deployment boundary hazard

The CLI can trigger deployment operations. It does not become the deployment controller.

## Observed correction

During the 2026-08-23 corpus review, the client boundary was narrowed to the
README's explicit statement. The earlier architecture wording could have made
the CLI appear to build, schedule, or run applications locally.

```claim
id: hazard.control-plane-ownership
kind: ASSERTED
statement: The client does not build, schedule, or run applications locally.
source: README.md
quote: "The client does not build, schedule, or run applications locally. Deployments are always based on committed GitHub refs or SHAs."
verified_at: 2026-08-23
verified_ref: 6e1e2dc37428eba4d99960e47e5d72b96fa4c601
```

```claim
id: hazard.direct-request-route
kind: MEASURED
statement: Direct deploy requests cross the control-plane boundary at the deploy request route.
command: grep -F -l '/v1/map-control/deploy/request' src/main.rs
expect: "src/main.rs"
match: exact
verified_at: 2026-08-23
verified_ref: 6e1e2dc37428eba4d99960e47e5d72b96fa4c601
```

Do not infer local scheduling, build execution, route mutation, or evidence ownership from a CLI command.
Read the control-plane corpus for those responsibilities.
