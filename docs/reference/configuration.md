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

These rules run only when a `[rules]` table adopts them, for the rule or for its group: `image/floating-tag`, `image/unpinned-action`, `script/log-vocabulary`, `docs/headcount`, `docs/stack-table`, and `docs/principles`. Their input exists in most repositories.

A line opts out of every rule with `tabrules:allow`, in an HTML comment in Markdown or in a comment in code.
