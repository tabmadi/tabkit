# The nixosTest of the binary cache, per ADR-0600. A builder pushes a path, and a client fetches it with the key of
# the cache. The test proves that a push needs a token, that a client refuses a path that the key did not sign, and
# that the mirror serves a path of its upstream.
{ pkgs, module }:
let
  # Throwaway keys for the test only. A real cache reads its keys from files at run time.
  keys = pkgs.runCommand "nix-cache-test-keys" { nativeBuildInputs = [ pkgs.nix ]; } ''
    mkdir -p $out && cd $out
    for name in cache mirror upstream; do
      nix --extra-experimental-features nix-command key generate-secret --key-name $name-1 > $name.key
      nix --extra-experimental-features nix-command key convert-secret-to-public < $name.key > $name.pub
    done
    echo test-token-for-the-binary-cache-0123456789 > token
    echo test-access > access
    echo test-secret > secret
  '';
  pub = name: builtins.readFile "${keys}/${name}.pub";

  ipOf = nodes: name: nodes.${name}.networking.primaryIPAddress;
  hosts = nodes: {
    networking.extraHosts = ''
      ${ipOf nodes "cache"} cache mirror s3.cache
      ${ipOf nodes "upstream"} upstream
    '';
  };
in
pkgs.testers.runNixOSTest {
  name = "nix-cache";

  nodes = {
    cache =
      { nodes, ... }:
      {
        imports = [
          module
          (hosts nodes)
        ];
        virtualisation.memorySize = 2048;
        # The keys reach the cache at run time, as real secrets do.
        systemd.tmpfiles.rules = map (f: "C /run/test/${f} 0444 root root - ${keys}/${f}") [
          "cache.key"
          "mirror.key"
          "token"
          "access"
          "secret"
        ];
        tabnix.nixCache = {
          enable = true;
          domain = "cache";
          acme = false;
          # The test VMs have no internet, so the OIDC discovery of GitHub is out of reach. The pushes use the token.
          oidc = false;
          githubOwners = [ "example" ];
          signingKeyFile = "/run/test/cache.key";
          apiTokenFile = "/run/test/token";
          s3 = {
            accessKeyFile = "/run/test/access";
            secretKeyFile = "/run/test/secret";
          };
          mirror = {
            domain = "mirror";
            signingKeyFile = "/run/test/mirror.key";
            upstreams = [ "http://upstream:5000" ];
            upstreamKeys = [ (pub "upstream") ];
          };
        };
      };

    # An upstream cache that the mirror fetches from, in place of the public cache.
    upstream =
      { nodes, ... }:
      {
        imports = [ (hosts nodes) ];
        systemd.tmpfiles.rules = [ "C /run/test/upstream.key 0400 root root - ${keys}/upstream.key" ];
        services.harmonia.cache = {
          enable = true;
          signKeyPaths = [ "/run/test/upstream.key" ];
          settings.bind = "[::]:5000";
        };
        networking.firewall.allowedTCPPorts = [ 5000 ];
      };

    builder =
      { nodes, ... }:
      {
        imports = [ (hosts nodes) ];
        environment.systemPackages = [ pkgs.niks3 ];
        systemd.tmpfiles.rules = [ "C /run/test/token 0400 root root - ${keys}/token" ];
      };

    client =
      { nodes, ... }:
      {
        imports = [ (hosts nodes) ];
      };
  };

  testScript = ''
    start_all()
    cache.wait_for_unit("seaweedfs.service")
    cache.wait_for_unit("niks3.service")
    cache.wait_for_unit("nginx.service")
    cache.wait_for_unit("ncps.service")
    upstream.wait_for_unit("harmonia.service")
    builder.wait_for_unit("multi-user.target")
    client.wait_for_unit("multi-user.target")

    # A path that exists only on the builder, and one that exists only on the upstream.
    # A path made by a derivation, not by `nix-store --add`. A content-addressed path proves itself and needs no
    # signature, so only an input-addressed path shows that the key matters.
    def build(machine, name):
        expr = (
            f"derivation {{ name = \"{name}\"; system = builtins.currentSystem; builder = \"/bin/sh\";"
            f" args = [ \"-c\" \"echo {name}-$RANDOM > $out\" ]; }}"
        )
        return machine.succeed(f"nix-build --no-out-link -E '{expr}' --argstr unused $RANDOM").strip()

    path = build(builder, "built-on-the-builder")
    upstream_path = build(upstream, "built-on-the-upstream")
    push = "niks3 push --server-url http://cache"

    with subtest("A push without a token is refused"):
        builder.fail(f"NIKS3_AUTH_TOKEN_FILE=/dev/null {push} {path}")
        client.fail(f"curl -sf http://cache/$(basename {path} | cut -c1-32).narinfo")

    with subtest("A push with the token stores the path, signed by the key of the cache"):
        builder.succeed(f"{push} --auth-token-path /run/test/token {path}")
        info = client.succeed(f"curl -sf http://cache/$(basename {path} | cut -c1-32).narinfo")
        assert "Sig: cache-1:" in info, info

    with subtest("A client that trusts the key fetches the path, and builds nothing"):
        client.succeed(
            f"nix copy --from http://cache {path} --option trusted-public-keys '${pub "cache"}'"
            " --extra-experimental-features nix-command"
        )
        client.succeed(f"test -e {path}")

    with subtest("A client that does not trust the key refuses the path"):
        client.succeed(f"nix-store --delete {path}")
        client.fail(
            f"nix copy --from http://cache {path} --option trusted-public-keys '${pub "upstream"}'"
            " --extra-experimental-features nix-command"
        )

    with subtest("The mirror serves a path of its upstream, signed by its own key"):
        client.succeed(
            f"nix copy --from http://mirror {upstream_path} --option trusted-public-keys '${pub "mirror"}'"
            " --extra-experimental-features nix-command"
        )
        client.succeed(f"test -e {upstream_path}")
  '';
}
