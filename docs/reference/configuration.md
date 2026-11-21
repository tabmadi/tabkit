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

A line opts out of every rule with `tabrules:allow`, in an HTML comment in Markdown or in a comment in code.
