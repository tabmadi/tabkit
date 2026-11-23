# An nginx origin behind the Cloudflare proxy, per ADR-0300. It ends TLS with the origin certificate and answers only
# a peer in the Cloudflare ranges. The module exposes the ranges, so the host can filter the same sources.
{
  config,
  lib,
  ...
}:
let
  inherit (lib) mkOption types;

  cfg = config.tabkit.cloudflareOrigin;
  sources = cfg.ranges.ipv4 ++ cfg.ranges.ipv6 ++ cfg.extraSources;
in
{
  options.tabkit.cloudflareOrigin = {
    enable = mkOption {
      type = types.bool;
      default = false;
      description = "Whether nginx serves the domain as a Cloudflare origin.";
    };
    domain = mkOption {
      type = types.str;
      example = "example.com";
      description = "The main domain of the origin.";
    };
    aliases = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "www.example.com" ];
      description = "The other domains of the origin.";
    };
    upstream = mkOption {
      type = types.str;
      example = "http://127.0.0.1:3000";
      description = "The local service that receives each accepted request.";
    };
    certificateFile = mkOption {
      type = types.str;
      example = "/run/secrets/origin.crt";
      description = "The Cloudflare origin certificate, as a path at run time.";
    };
    keyFile = mkOption {
      type = types.str;
      example = "/run/secrets/origin.key";
      description = "The private key of the origin certificate, as a path at run time outside the Nix store.";
    };
    ranges = mkOption {
      type = types.submodule {
        options = {
          ipv4 = mkOption {
            type = types.listOf types.str;
            description = "The IPv4 ranges of Cloudflare.";
          };
          ipv6 = mkOption {
            type = types.listOf types.str;
            description = "The IPv6 ranges of Cloudflare.";
          };
        };
      };
      default = lib.importJSON ./data/cloudflare-ranges.json;
      defaultText = lib.literalMD "the ranges in `nix/nixos/data/cloudflare-ranges.json`";
      description = "The source ranges of the Cloudflare proxy.";
    };
    extraSources = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Other sources that the origin accepts, such as the stand-in proxy of a test.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = !lib.hasPrefix builtins.storeDir cfg.keyFile;
        message = "tabkit.cloudflareOrigin: a secret is a path at run time, outside the Nix store.";
      }
    ];

    services.nginx = {
      enable = true;
      recommendedProxySettings = true;
      recommendedTlsSettings = true;
      virtualHosts.${cfg.domain} = {
        serverAliases = cfg.aliases;
        default = true;
        onlySSL = true;
        listen = [
          {
            addr = "0.0.0.0";
            port = 443;
            ssl = true;
          }
        ];
        sslCertificate = "/run/credentials/nginx.service/origin.crt";
        sslCertificateKey = "/run/credentials/nginx.service/origin.key";
        extraConfig = lib.concatMapStrings (source: "allow ${source};\n") sources + ''
          deny all;
        '';
        locations."/".proxyPass = cfg.upstream;
      };
    };

    systemd.services.nginx.serviceConfig.LoadCredential = [
      "origin.crt:${cfg.certificateFile}"
      "origin.key:${cfg.keyFile}"
    ];

    networking.firewall.allowedTCPPorts = [ 443 ];
  };
}
