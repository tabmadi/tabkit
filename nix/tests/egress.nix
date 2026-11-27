# The nixosTest of the egress module, per ADR-1300. At the proxy level, the client reaches the upstream only through
# the proxy, and the Nix daemon fetches through it. At the airgap level, Nix reads only the internal cache, and a fetch
# from outside fails. The module refuses each setting that would leak a secret or the public cache.
{ pkgs, module }:
let
  inherit (pkgs) lib;
  content = "egress-test\n";
  hash = builtins.hashString "sha256" content;

  keys = pkgs.runCommand "egress-test-keys" { nativeBuildInputs = [ pkgs.nix ]; } ''
    mkdir -p $out
    nix --extra-experimental-features nix-command key generate-secret --key-name cache-1 > $out/cache.key
    nix --extra-experimental-features nix-command key convert-secret-to-public < $out/cache.key > $out/cache.pub
  '';
  cachePub = builtins.readFile "${keys}/cache.pub";

  # The messages of the failed assertions, for one setting of the module.
  refusals =
    settings:
    let
      machine = pkgs.nixos {
        imports = [ module ];
        tabkit.egress = settings // {
          enable = true;
        };
        boot.loader.grub.enable = false;
        fileSystems."/" = {
          device = "/dev/vda";
          fsType = "ext4";
        };
        system.stateVersion = "26.05";
      };
    in
    map (a: a.message) (lib.filter (a: !a.assertion) machine.config.assertions);
  refused = settings: refusals settings != [ ];

  hosts = nodes: {
    networking.extraHosts = ''
      ${nodes.proxy.networking.primaryIPAddress} proxy
      ${nodes.upstream.networking.primaryIPAddress} upstream
      ${nodes.cache.networking.primaryIPAddress} cache
    '';
  };
  fetch = "NIX_REMOTE=daemon nix-build --no-out-link --option connect-timeout 5 --option download-attempts 1 -E 'import <nix/fetchurl.nix> { url = \"http://upstream/file\"; sha256 = \"${hash}\"; }'";
in
assert refused { level = "proxy"; };
assert refused {
  level = "direct";
  proxy = "http://proxy:3128";
};
assert refused {
  level = "proxy";
  proxy = "http://user:secret@proxy:3128";
};
assert refused {
  level = "proxy";
  environmentFile = "${keys}/cache.key";
};
assert refused {
  level = "airgap";
  substituters = [ "https://cache.nixos.org" ];
};
assert
  refusals {
    level = "proxy";
    proxy = "http://proxy:3128";
  } == [ ];
pkgs.testers.runNixOSTest {
  name = "egress";

  nodes = {
    # The upstream is only on the second network, so only the proxy reaches it.
    upstream = {
      virtualisation.vlans = [ 2 ];
      services.nginx = {
        enable = true;
        virtualHosts.upstream.root = pkgs.writeTextDir "file" content;
      };
      networking.firewall.allowedTCPPorts = [ 80 ];
    };

    proxy =
      { nodes, ... }:
      {
        imports = [ (hosts nodes) ];
        virtualisation.vlans = [
          1
          2
        ];
        services.squid = {
          enable = true;
          # The test networks use the documentation range of IPv6, which the default local ranges leave out.
          extraConfig = ''
            acl testnet src 2001:db8::/32
            http_access allow testnet
          '';
        };
        networking.firewall.allowedTCPPorts = [ 3128 ];
      };

    client =
      { nodes, ... }:
      {
        imports = [
          module
          (hosts nodes)
        ];
        virtualisation.vlans = [ 1 ];
        environment.systemPackages = [ pkgs.curl ];
        tabkit.egress = {
          enable = true;
          level = "proxy";
          proxy = "http://proxy:3128";
          noProxy = [ "forge.internal" ];
        };
      };

    # The internal cache and the airgap client share a third network with nothing else.
    cache = {
      virtualisation.vlans = [ 3 ];
      systemd.tmpfiles.rules = [ "C /run/test/cache.key 0400 root root - ${keys}/cache.key" ];
      services.harmonia.cache = {
        enable = true;
        signKeyPaths = [ "/run/test/cache.key" ];
        settings.bind = "[::]:5000";
      };
      networking.firewall.allowedTCPPorts = [ 5000 ];
    };

    closed =
      { nodes, ... }:
      {
        imports = [
          module
          (hosts nodes)
        ];
        virtualisation.vlans = [ 3 ];
        tabkit.egress = {
          enable = true;
          level = "airgap";
          substituters = [ "http://cache:5000" ];
          trustedPublicKeys = [ cachePub ];
        };
      };
  };

  testScript = ''
    start_all()
    upstream.wait_for_unit("nginx.service")
    proxy.wait_for_unit("squid.service")
    proxy.wait_for_open_port(3128)
    cache.wait_for_open_port(5000)
    client.wait_for_unit("multi-user.target")
    closed.wait_for_unit("multi-user.target")

    with subtest("The egress file holds the level, the proxy, and the bypass list"):
        env = client.succeed("cat /etc/tabkit/egress.env")
        assert "TABKIT_EGRESS=proxy" in env, env
        assert "HTTPS_PROXY=http://proxy:3128" in env, env
        assert "NO_PROXY=localhost,127.0.0.1," in env and ",forge.internal" in env, env
        client.succeed("systemctl show nix-daemon -p EnvironmentFiles | grep -q /etc/tabkit/egress.env")

    with subtest("The client cannot reach the upstream directly"):
        client.fail("curl --noproxy '*' -sf -m 5 http://upstream/file")
        client.succeed("curl -sf -m 10 -x http://proxy:3128 http://upstream/file | grep -q egress-test")

    with subtest("The Nix daemon fetches through the proxy"):
        client.succeed(r"""${fetch}""")
        proxy.succeed("grep -q 'http://upstream/file' /var/log/squid/access.log")

    # A path that only the cache built. It is input-addressed, so only the key of the cache can vouch for it.
    path = cache.succeed(
        "nix-build --no-out-link -E 'derivation { name = \"internal\"; system = builtins.currentSystem;"
        " builder = \"/bin/sh\"; args = [ \"-c\" \"echo internal-$RANDOM > $out\" ]; }'"
    ).strip()

    with subtest("At the airgap level, Nix reads only the internal cache"):
        substituters = closed.succeed("nix --extra-experimental-features nix-command config show substituters").strip()
        assert substituters == "http://cache:5000", substituters
        closed.succeed(f"NIX_REMOTE=daemon nix-store -r {path}")
        closed.succeed(f"test -e {path}")

    with subtest("At the airgap level, a fetch from outside fails"):
        closed.fail(r"""${fetch}""")
  '';
}
