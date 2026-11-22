# The push to the binary cache of ADR-0600. It runs after `just ci` in a CI job, and pushes only the paths that the job
# built itself: a path from a public cache is never pushed again. Without a cache URL or an OIDC token it does nothing,
# so a fork, a pull request, and a project without a cache stay unchanged.
{ pkgs }:
pkgs.writeShellApplication {
  name = "nix-cache-push";
  runtimeInputs = [
    pkgs.nix
    pkgs.jq
    pkgs.curl
    pkgs.coreutils
    pkgs.niks3
  ];
  text = ''
    url="''${NIX_CACHE_URL:-}"
    if [ -z "$url" ]; then
      echo "NIX_CACHE_URL is not set, so nothing is pushed."
      exit 0
    fi
    if [ -z "''${ACTIONS_ID_TOKEN_REQUEST_URL:-}" ] || [ -z "''${ACTIONS_ID_TOKEN_REQUEST_TOKEN:-}" ]; then
      echo "This job has no OIDC token, so nothing is pushed. A push needs 'id-token: write' on the job."
      exit 0
    fi

    if ! curl -sf --max-time 10 "$url/nix-cache-info" > /dev/null; then
      echo "::warning::The binary cache at $url does not answer, so nothing is pushed. The result of the CI gate does not change."
      exit 0
    fi

    # Every dev shell of the flake, so a later job finds each one in the cache.
    system="$(nix eval --raw --impure --expr builtins.currentSystem)"
    for shell in $(nix eval --json ".#devShells.$system" --apply builtins.attrNames | jq -r '.[]'); do
      nix build --no-link ".#devShells.$system.$shell"
    done

    # The OIDC token of GitHub Actions, with the cache as its audience. niks3 asks again before the token expires.
    token_script="$(mktemp)"
    trap 'rm -f "$token_script"' EXIT
    cat > "$token_script" <<EOF
    #!/usr/bin/env bash
    set -euo pipefail
    token=\$(curl -sSf -H "Authorization: bearer \$ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
      "\$ACTIONS_ID_TOKEN_REQUEST_URL&audience=$url" | jq -r .value)
    jq -n --arg token "\$token" --arg expires "\$(date -u -d '+4 minutes' +%Y-%m-%dT%H:%M:%SZ)" \
      '{token: \$token, expires_at: \$expires}'
    EOF
    chmod +x "$token_script"

    # A path that this job built is ultimate. A path that came from a cache is not. The push is advisory: a cache
    # that is down costs the next job time, so it never fails this one.
    nix path-info --all --json \
      | jq -r 'to_entries[] | select(.value.ultimate == true) | .key | select(endswith(".drv") | not)' \
      > "$token_script.paths"
    echo "Pushing $(wc -l < "$token_script.paths") paths that this job built to $url"
    if ! timeout 600 niks3 push --server-url "$url" --auth-token-script "$token_script" --retry-timeout 30s --stdin \
      < "$token_script.paths" | jq -r 'select(.status == "error") | "push failed: \(.path): \(.message)"'; then
      echo "::warning::The push to the binary cache failed. The result of the CI gate does not change."
    fi
    rm -f "$token_script.paths"
  '';
}
