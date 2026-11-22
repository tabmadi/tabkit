# Simple English profile

The default rule set of tabkit. A missing or empty `.config/tabkit.toml` runs all of it.

| Part | Rule groups | Checks |
| --- | --- | --- |
| Words and punctuation | `simple`, `char` | Common words, one idea per sentence, at most 25 words, and a small set of punctuation marks |
| Banned constructs | `construct` | Intensifiers, hedges, meta-commentary, chronology, status words, and planned work |
| Comments | `comment` | Short blocks that state what the code cannot show |
| Decision records | `adr`, `annotation` | The shape of each ADR, its index, its references, and the recipes it names |
| Docs layout | `docs` | Genre directories, the docs root, and links to tracked files |
| Requirements | `requirement` | The form of each requirement, and a test that cites its ID |
| Repository | `workflow`, `image`, `script`, `version`, `just` | Thin workflows, pinned references, log output, version lines, and recipe docs |

`tabkit rules` prints every rule ID with its text. To leave a rule out, see [Configuration](configuration.md).
