# shellcheck shell=bash
# The cluster helpers for the shell scripts that a project keeps for itself. The commands of tabkit-ops are Rust.

# shellcheck disable=SC2317  # the guard's `return` is reached when re-sourced.
if [[ -n "${__CLUSTER_SH_LOADED:-}" ]]; then return 0 2>/dev/null || true; fi
__CLUSTER_SH_LOADED=1

source "$(dirname "${BASH_SOURCE[0]}")/bootstrap.sh"

# Loaded, not computed. kind reads the proxy variables from its own environment, and only kind can add the node's name to NO_PROXY.
# Without that, `kind create` stops on an EOF.
if [ -f "$ROOT/infra/local/proxy.local.env" ]; then
  set -a
  # shellcheck source=/dev/null
  . "$ROOT/infra/local/proxy.local.env"
  set +a
fi
CLUSTER="${CLUSTER:-platform}"
NS="${NS:-platform}"
DOMAIN="${DOMAIN:-dev.localtest.me}"
KIND_CONFIG="infra/local/kind.yaml"
REGISTRY="registry.localhost"
# A path, never a named volume: a forge cache can restore a directory. A warm from nothing is most of the cost of `cluster-up`, per ADR-0205.
ZOT_DATA="${ZOT_DATA:-${XDG_CACHE_HOME:-$HOME/.cache}/zot/${REGISTRY}}"
ZOT_IMAGE="ghcr.io/project-zot/zot-linux-amd64:v2.1.23"

# Checked when the file is sourced. The functions run as `$(cluster_ctx)`, where an exit ends only the subshell.
case "${TIER:-}" in
"" | base | full) ;;
*) fail "'${TIER}' is not a tier. Use \"base\" or \"full\"" ;;
esac

cluster_tier() { printf '%s' "$TIER"; }
# The tier is an argument, so a message naming a command names its tier too.
up_hint() {
  if [ "$TIER" = full ]; then printf 'just cluster-up full'; else printf 'just cluster-up'; fi
}
# The full tier's name includes the tier, so `docker ps` and `kubectl config get-contexts` both show which cluster is which.
cluster_name_of() {
  if [ "$1" = full ]; then printf '%s-full' "$CLUSTER"; else printf '%s' "$CLUSTER"; fi
}
cluster_name() { cluster_name_of "$TIER"; }
# A CI runner can run two jobs at once and lives longer than both. There, the registry and the cluster belong to the job that created them.
# The registry is the first host-level stage. It carries the job's name and acts as the lock.
ci_job() { printf '%s-%s-%s' "${GITHUB_RUN_ID:-}" "${GITHUB_JOB:-}" "${GITHUB_RUN_ATTEMPT:-}"; }
ci_owned() { [ -z "${CI:-}" ] || grep -qx "$1" "${RUNNER_TEMP:?}/kind-cluster" 2>/dev/null; }
registry_job() { docker inspect -f '{{index .Config.Labels "platform.ci-job"}}' "$REGISTRY" 2>/dev/null || true; }
# This job never replaces, resumes, or moves another job's registry and cluster. It waits for that job to finish.
ci_wait_for_runner() {
  local waited=0 held
  while :; do
    held="$(registry_job)"
    { [ -n "$held" ] && [ "$held" != "$(ci_job)" ]; } ||
      cluster_exists "$(cluster_name_of base)" || cluster_exists "$(cluster_name_of full)" || return 0
    [ "$waited" -lt 3600 ] || fail "another job has held this runner's registry and edge for an hour"
    [ "$waited" -gt 0 ] || step "waiting for another job to release this runner's registry and edge"
    sleep 30
    waited=$((waited + 30))
  done
}
other_tier() { if [ "$TIER" = full ]; then printf 'base'; else printf 'full'; fi; }
cluster_exists() { kind get clusters 2>/dev/null | grep -qx "$1"; }
# A tier-scoped verb that finds nothing reports it and names the tier that is up.
# The tiers are alternatives, so `no such cluster` usually means the wrong tier was named.
other_tier_hint() {
  local other
  other="$(other_tier)"
  cluster_exists "$(cluster_name_of "$other")" &&
    detail "the ${other} tier is up. To use it, run 'just cluster:${1} -- ${other}'"
  return 0
}
cluster_ctx() { printf 'kind-%s' "$(cluster_name)"; }

# True when this cluster's node runs, not only exists. Both tiers can exist at once.
# Only one can hold the edge ports, so only one serves.
cluster_running() {
  docker inspect -f '{{.State.Running}}' "${1}-control-plane" 2>/dev/null | grep -qx true
}

# The tier that is up, or nothing. A running tier wins over a created one.
# When the answer is not clear, it returns nothing, so the caller must name the cluster it wants.
detect_tier() {
  # One listing for both tiers: this runs on every source, and `kind get clusters` is a docker round-trip.
  # `if`, not `&&`, so a stopped cluster is an answer and not a non-zero status that ends the caller under `set -e`.
  local tier name clusters running=() present=()
  clusters="$(kind get clusters 2>/dev/null || true)"
  for tier in base full; do
    name="$(cluster_name_of "$tier")"
    printf '%s\n' "$clusters" | grep -qx "$name" || continue
    present+=("$tier")
    if cluster_running "$name"; then running+=("$tier"); fi
  done
  if [ "${#running[@]}" -eq 1 ]; then
    printf '%s' "${running[0]}"
  elif [ "${#running[@]}" -eq 0 ] && [ "${#present[@]}" -eq 1 ]; then
    printf '%s' "${present[0]}"
  else
    return 1
  fi
}

# The tier a script acts on. The order: TIER in the environment, then TIER_FROM_ARGV=1 for cluster.sh, then the tier that is up, then base.
# A hardcoded default fails with no clear error: every kubectl fails against a context that does not exist, and it looks like the platform is down.
if [ -z "${TIER:-}" ] && [ -z "${TIER_FROM_ARGV:-}" ]; then
  TIER="$(detect_tier || true)"
fi
TIER="${TIER:-base}"
# Exported, because stages call scripts that resolve the context from it.
# Without the export, `identity-seed.sh` seeds the inner loop while the full tier waits.
export TIER

k() { kubectl --context "$(cluster_ctx)" "$@"; }
h() { helm --kube-context "$(cluster_ctx)" "$@"; }

# Poll until a command succeeds. Custom readiness loops made the two tiers drift apart.
wait_for() { # <what> <seconds> <cmd> [args]
  local what="$1" budget="$2"
  shift 2
  local waited=0
  until "$@" >/dev/null 2>&1; do
    [ "$waited" -ge "$budget" ] && fail "timed out after ${budget}s waiting for ${what}"
    sleep 2
    waited=$((waited + 2))
  done
}

# `build` is the offline path and needs the dependency's repo registered in the caller's helm config.
# `update` resolves it from Chart.yaml. Developer machines pass on the first, and clean runners only on the second.
chart_deps() { helm dependency build "$1" >/dev/null 2>&1 || helm dependency update "$1" >/dev/null; }

# An absent tool looks like an answer: a failed `kind get clusters` looks like a cluster that was never created.
# Every probe below has the same problem. So check the tools first, and `not found` cannot mean `not created`.
require_tools() {
  local tool
  for tool in "$@"; do
    command -v "$tool" >/dev/null 2>&1 ||
      fail "${tool} not found on PATH. Run this through just, inside the dev shell of flake.nix, which puts the pinned toolchain there"
  done
}

# Fail unless the probe can run. Otherwise an unreachable cluster looks like `not deployed` to every idempotence guard.
require_cluster() {
  require_tools kubectl
  k cluster-info >/dev/null 2>&1 ||
    fail "cluster $(cluster_ctx) is not reachable. Run '$(up_hint)' first"
}

# The committed ApplicationSet named apps after the values file, and now it removes the extension. So both names exist.
# A wrong guess leaves auto-sync on, and Argo reverts the deploy.
argo_service_app() {
  local name
  for name in "local-service-${1}" "local-service-${1}.yaml"; do
    if k -n argocd get application.argoproj.io "$name" >/dev/null 2>&1; then
      echo "$name"
      return 0
    fi
  done
}
