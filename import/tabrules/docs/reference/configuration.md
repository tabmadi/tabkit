# Configuration

tabrules reads `.config/tabrules.toml`. An empty or missing file runs every rule with its defaults. A key sets a path or turns a rule off:

```toml
exclude = ["vendor/**"]
disable = ["construct/status", "docs"]

[adr]
foreign = ["games"]
index-matches-decides = false

[docs]
genre-dirs = ["adr", "guide", "reference"]
root-files = ["README.md", "tool-register.md"]

[messages]
dir = "apps/frontend/src/messages"

[requirements]
tests = ["**/*.test.ts", "e2e/**/*.spec.ts"]
```

A `[rules]` table sets the citation and the paths of one rule or one group. A finding prints the citation at its end:

```toml
[rules."workflow/thin"]
cite = "ADR-0101"
exempt = ["**/check.yml"]

[rules.image]
cite = "ADR-0111"
```

| Rule | Path keys |
| --- | --- |
| `workflow/thin` | `paths`, `exempt` |
| `image/floating-tag` | `paths`, `exempt` |
| `image/unpinned-action` | `paths` |
| `script/log-vocabulary` | `paths`, `exempt` |
| `frontend/logical-property` | `paths` |
| `frontend/contrast` | `theme` |
| `frontend/fixture` | `paths`, `seams`, `sdks` |
| `sql/pii-tag` | `paths` |
| `alert/severity` | `paths`, `allowed`, `watchdog` |
| `api/audience` | `specs`, `values` |
| `edge/api-wildcard` | `access-rules` |
| `naming/resource` | `roles-doc`, `environments`, `local-tier`, `inventory`, `sops`, `overlays` |
| `helm/dependency-fallback` | `paths` |
| `helm/app-version` | `paths` |
| `project/identity` | `footprints`, `init-script`, `project-file`, `inventory`, `apex`, `exempt` |
| `edge/auth-inline` | `values`, `required`, `source` |
| `edge/l7-routes` | `paths` |
| `edge/strip-headers` | `manifests`, `chart`, `forward-auth`, `strip` |
| `gitops/env-scope` | `root` |
| `gitops/parity` | `overlay`, `allowlist` |
| `service/ports` | `registry`, `reserved` |
| `service/contract` | `values-root`, `linter-config` |
| `service/databases` | `values`, `local-secret`, `network-policy`, `secret-docs` |
| `service/workspace-copies` | `dockerfile` |
| `go/call-args` | `paths`, `exempt` |
| `money/spec` | `specs`, `component` |
| `money/no-float` | `skip` |
| `money/typescript` | `skip` |
| `kube/resource-governance` | `governance`, `charts`, `service-chart`, `shared-values`, `default-namespace`, `namespaces`, `chart-set`, `service-set`, `cpu-limit-allow` |
| `adr/adoption-components` | `inventory`, `adoption`, `section` |
| `adr/deferral-register` | `register`, `adr-dir`, `exempt` |
| `adr/tool-register` | `register`, `adr-dir` |
| `docs/stack-table` | `readme` |
| `docs/principles` | `foundations`, `adr-dir` |
| `docs/headcount` | `paths`, `exempt` |
| `gen/rules-index` | `rules-path`, `baseline-path`, `adr-dir`, `rules-intro`, `rules-outro`, `baseline-intro`, `security-areas` |
| `gen/data-classes` | `output`, `retention`, `header`, `footer`, `migrations` |
| `gen/shared-components` | `source`, `specs` |
| `gen/zod` | `out-dir`, `specs`, `skip`, `header` |
| `gen/admin` | `out-dir`, `specs`, `skip`, `header` |
| `gen/build-path` | `doc`, `workflows`, `exempt` |
| `affected` | `global-files`, `global-prefixes`, `runtime-ignore` |

A generator writes nothing until its first key is set: `rules-path`, `output`, `source`, `out-dir`, or `doc`.

These rules run only when a `[rules]` table adopts them, for the rule or for its group: `image/floating-tag`, `image/unpinned-action`, `script/log-vocabulary`, `docs/headcount`, `docs/stack-table`, `docs/principles`, `project/identity`, `service/node-scope`, `service/contract`, `helm/app-version`, `sql/pii-tag`, `api/audience`, `api/prefixes`, `go/call-args`, `go/activity-register`, `go/service-deps`, `money/spec`, `money/no-float`, `money/typescript`, and `kube/resource-governance`. Their input exists in most repositories.

`tabrules check --only money,go/call-args` runs only those rule IDs or groups. An opt-in rule still needs its `[rules]` table.

Five rules run only when `--only` names them or their group: `kube/resource-governance`, `go/service-deps`, `edge/strip-headers`, `helm/app-version`, and `project/identity`. They call a slow program, need vendored charts, or judge one moment of a repository.

A line opts out of every rule with `tabrules:allow`, in an HTML comment in Markdown or in a comment in code.
