# Platform layout

`tabkit-ops` reads a project through these paths, per [ADR-1200](../adr/1200-platform-commands.md). A project that moves one of them breaks the commands that read it.

| Path | Holds | Read by |
| --- | --- | --- |
| `infra/helm/platform/<chart>/` | The platform charts | `gen-image-allowlist`, `cluster`, `platform-deploy` |
| `infra/helm/service/` | The chart of every service | `gen-image-allowlist`, `service-deploy` |
| `infra/gitops/platform/shared-values.yaml` | The values of every environment | `gen-image-allowlist` |
| `infra/gitops/platform/<env>/values.yaml` | The platform values of one environment | `gen-image-allowlist`, `promote-dev-staging`, `promote-prod` |
| `infra/gitops/services/<env>/values/<service>.yaml` | The image pins of one service | `promote-dev-staging`, `promote-prod`, `ci-prune-registry` |
| `infra/gitops/platform/image-allowlist.yaml` | The generated allow-list | `gen-image-allowlist` |
| `infra/local/image-refs.txt` | The generated warm list | `gen-image-allowlist`, `cluster` |
| `infra/local/deps.yaml`, `infra/local/mock.yaml` | The local stand-ins | `gen-image-allowlist`, `cluster` |
| `infra/auth/cosign/signing-key.enc.yaml` | The encrypted signing key | `ci-sign` |
| `scripts/lib/ports.sh` | The registry of local ports | `service-dev`, `svc-apply` |
| `scripts/identity-seed.sh`, `scripts/dep-apply.sh` | The project's own seed and dependency steps | `cluster`, `service-deploy` |
