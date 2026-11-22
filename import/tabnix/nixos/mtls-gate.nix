# The mTLS gate, per ADR-0300. nginx admits a request only with a client certificate from one CA,
# and the firewall opens the port only to the listed source addresses.
{ config, lib, ... }:
let
  inherit (lib) mkOption types;

  cfg = config.tabnix.mtlsGate;

  isIPv6 = address: lib.hasInfix ":" address;

  # One nftables rule for each address family that the gate lists.
  firewallRules =
    gate:
    let
      v4 = builtins.filter (a: !isIPv6 a) gate.allowFrom;
      v6 = builtins.filter isIPv6 gate.allowFrom;
      rule =
        family: addresses:
        lib.optionalString (addresses != [ ])
          "${family} saddr { ${lib.concatStringsSep ", " addresses} } tcp dport ${toString gate.port} accept";
    in
    lib.concatStringsSep "\n" (
      lib.filter (r: r != "") [
        (rule "ip" v4)
        (rule "ip6" v6)
      ]
    );

  gateOptions =
    { name, ... }:
    {
      options = {
        address = mkOption {
          type = types.str;
          example = "10.20.0.30";
          description = "The address that the gate listens on.";
        };
        port = mkOption {
          type = types.port;
          example = 8443;
          description = "The TCP port that the gate listens on.";
        };
        upstream = mkOption {
          type = types.str;
          example = "http://127.0.0.1:8080";
          description = "The URL of the service behind the gate. It must listen only on the loopback address.";
        };
        certificate = mkOption {
          type = types.path;
          description = "The server certificate of the gate. A certificate is public, so it can be in the Nix store.";
        };
        key = mkOption {
          type = types.str;
          example = "/run/secrets/${name}.key";
          description = "The absolute path of the server key at run time. It must be outside the Nix store.";
        };
        clientCa = mkOption {
          type = types.path;
          description = "The CA certificate that signs every client certificate that the gate admits.";
        };
        allowFrom = mkOption {
          type = types.nonEmptyListOf types.str;
          example = [ "10.20.0.10" ];
          description = "The source addresses or CIDR ranges that the firewall admits to the port.";
        };
      };
    };
in
{
  options.tabnix.mtlsGate = mkOption {
    type = types.attrsOf (types.submodule gateOptions);
    default = { };
    description = "The mTLS gates of the machine, by name.";
  };

  config = lib.mkIf (cfg != { }) {
    assertions = [
      {
        assertion = config.networking.nftables.enable;
        message = "tabnix.mtlsGate needs networking.nftables.enable, because it adds nftables rules.";
      }
    ]
    ++ lib.mapAttrsToList (name: gate: {
      assertion = lib.hasPrefix "/" gate.key && !lib.hasPrefix builtins.storeDir gate.key;
      message = "tabnix.mtlsGate.${name}.key must be an absolute path outside the Nix store.";
    }) cfg;

    networking.firewall.extraInputRules = lib.concatStringsSep "\n" (
      lib.mapAttrsToList (_: firewallRules) cfg
    );

    # nginx runs as its own user. systemd reads each key as root and gives nginx a private copy,
    # so the key file itself can stay readable only by root.
    systemd.services.nginx.serviceConfig.LoadCredential = lib.mapAttrsToList (
      name: gate: "mtls-gate-${name}.key:${gate.key}"
    ) cfg;

    services.nginx = {
      enable = true;
      recommendedTlsSettings = lib.mkDefault true;
      recommendedProxySettings = lib.mkDefault true;
      virtualHosts = lib.mapAttrs' (
        name: gate:
        lib.nameValuePair "mtls-gate-${name}" {
          listen = [
            {
              addr = gate.address;
              inherit (gate) port;
              ssl = true;
            }
          ];
          onlySSL = true;
          sslCertificate = gate.certificate;
          sslCertificateKey = "/run/credentials/nginx.service/mtls-gate-${name}.key";
          extraConfig = ''
            ssl_client_certificate ${gate.clientCa};
            ssl_verify_client on;
            ssl_verify_depth 1;
          '';
          locations."/" = {
            proxyPass = gate.upstream;
            # The upstream trusts these headers, so the gate always sets them and a client cannot.
            extraConfig = ''
              proxy_set_header X-Client-Subject $ssl_client_s_dn;
              proxy_set_header X-Client-Verify $ssl_client_verify;
            '';
          };
        }
      ) cfg;
    };
  };
}
