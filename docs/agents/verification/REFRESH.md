---
title: MAP CLI corpus refresh
answers: How can an agent refresh this corpus while automation remains deferred?
last_verified: 2026-08-23
verified_ref: 6e1e2dc37428eba4d99960e47e5d72b96fa4c601
refresh: quarterly
---

# Refresh

This corpus defers automatic runner work to the later phase.

```claim
id: refresh.manual-boundary
kind: MEASURED
statement: No tracked file under .github or scripts names the agent-docs corpus markers used by an automatic refresh entrypoint.
command: |
  files="$(git ls-files .github scripts)"
  if [ -z "$files" ]; then printf '%s' ""; exit 0; fi
  err="$(mktemp)"
  trap 'rm -f "$err"' EXIT
  set +e
  out="$(git grep -Il -e 'docs/agents' -e 'CORPUS-SPEC.md' -e 'verified_ref' -- $files 2>"$err")"
  rc=$?
  set -e
  if [ "$rc" -gt 1 ] || [ -s "$err" ]; then cat "$err" >&2; exit 2; fi
  printf '%s' "$out"
expect: ""
match: exact
allow_empty: true
verified_at: 2026-08-23
verified_ref: 6e1e2dc37428eba4d99960e47e5d72b96fa4c601
```

```claim
id: refresh.spec-present
kind: MEASURED
statement: The corpus specification is present at the repository-local path.
command: test -f docs/agents/CORPUS-SPEC.md && printf '%s\n' present
expect: "present"
match: exact
verified_at: 2026-08-23
verified_ref: 6e1e2dc37428eba4d99960e47e5d72b96fa4c601
```

Refresh the measured claims after source, route, fixture, or packaging changes.
Update verified_at and verified_ref for every claim that changes.
