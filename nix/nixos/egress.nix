# The egress of a machine, per ADR-1300. It writes /etc/tabkit/egress.env, loads it into each unit that fetches,
# installs the site CA, and at the internal and airgap levels keeps Nix on the internal caches only.
{
  config,
  lib,
  ...
}:
let
  inherit (lib) mkOption types;

  cfg = config.tabkit.egress;
  file = "/etc/tabkit/egress.env";
  # The same base as the bypass function of tabkit-ops.
  base = [
    "localhost"
    "127.0.0.1"
    "::1"
    ".localhost"
    ".localtest.me"
    ".svc"
    ".cluster.local"
    "10.0.0.0/8"
    "172.16.0.0/12"
    "192.168.0.0/16"
    "fc00::/7"
  ];
  bypass = lib.concatStringsSep "," (lib.unique (base ++ cfg.noProxy));
  closed = lib.elem cfg.level [
    "internal"
    "airgap"
  ];
  proxyLines = lib.optionalString (cfg.proxy != null) ''
    HTTP_PROXY=${cfg.proxy}
    HTTPS_PROXY=${cfg.proxy}
    http_proxy=${cfg.proxy}
    https_proxy=${cfg.proxy}
  '';
in
{
  options.tabkit.egress = {
    enable = mkOption {
      type = types.bool;
      default = false;
      description = "Whether the machine sets its egress from these options.";
    };
    level = mkOption {
      type = types.enum [
        "direct"
        "proxy"
        "internal"
        "airgap"
      ];
      default = "direct";
      description = "The egress level of ADR-1300.";
    };
    proxy = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "http://proxy.example.net:3128";
      description = "The proxy URL without credentials, at the proxy level only. The file of `environmentFile` holds a URL with credentials.";
    };
    environmentFile = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/run/secrets/egress.env";
      description = "A file outside the Nix store with more variables, such as a proxy URL with credentials. Each unit loads it after the egress file.";
    };
    noProxy = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "forge.internal" ];
      description = "The names and ranges of the site that every client reaches directly. The module adds the base list.";
    };
    caFile = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = "The site CA, for a proxy that inspects TLS or for internal services.";
    };
    substituters = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "https://cache.internal" ];
      description = "The internal Nix caches. At the internal and airgap levels they are the only substituters.";
    };
    trustedPublicKeys = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "The keys of the internal Nix caches.";
    };
    units = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "forgejo" ];
      description = "More systemd units that fetch. The module always sets nix-daemon, and docker when Docker is on.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.level == "proxy" -> (cfg.proxy != null || cfg.environmentFile != null);
        message = "tabkit.egress: the proxy level needs `proxy` or `environmentFile`.";
      }
      {
        assertion = cfg.level != "proxy" -> cfg.proxy == null;
        message = "tabkit.egress: `proxy` is set, so the level is `proxy`.";
      }
      {
        assertion = cfg.proxy == null || !(lib.hasInfix "@" cfg.proxy);
        message = "tabkit.egress: a proxy URL with credentials goes into `environmentFile`, outside the Nix store.";
      }
      {
        assertion = cfg.environmentFile == null || !(lib.hasPrefix builtins.storeDir cfg.environmentFile);
        message = "tabkit.egress: `environmentFile` must be outside the Nix store.";
      }
      {
        assertion = closed -> !(lib.any (lib.hasInfix "cache.nixos.org") cfg.substituters);
        message = "tabkit.egress: the ${cfg.level} level reads only internal caches.";
      }
    ];

    environment.etc."tabkit/egress.env".text = ''
      TABKIT_EGRESS=${cfg.level}
      ${proxyLines}NO_PROXY=${bypass}
      no_proxy=${bypass}
    '';

    systemd.services =
      lib.genAttrs
        ([ "nix-daemon" ] ++ lib.optional config.virtualisation.docker.enable "docker" ++ cfg.units)
        (_: {
          serviceConfig.EnvironmentFile = [
            file
          ]
          ++ lib.optional (cfg.environmentFile != null) "-${cfg.environmentFile}";
        });

    security.pki.certificateFiles = lib.optional (cfg.caFile != null) cfg.caFile;

    nix.settings =
      if closed then
        {
          substituters = lib.mkForce cfg.substituters;
          trusted-public-keys = lib.mkForce cfg.trustedPublicKeys;
        }
      else
        {
          extra-substituters = cfg.substituters;
          extra-trusted-public-keys = cfg.trustedPublicKeys;
        };
  };
}
