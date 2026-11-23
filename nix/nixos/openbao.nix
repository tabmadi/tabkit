# OpenBao for one machine, per ADR-0300: Raft storage, TLS 1.3, a file audit log, and scheduled snapshots that are
# encrypted before they reach a disk. The job signs in with a client certificate, so it holds no long-lived token.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib) mkOption types;

  cfg = config.tabkit.openbao;
  snapshot = cfg.snapshot;
  bao = "${cfg.package}/bin/bao";

  serverConfig = pkgs.writeText "openbao.hcl" ''
    ui = false
    disable_mlock = true
    api_addr = "${cfg.apiAddress}"
    cluster_addr = "https://127.0.0.1:${toString cfg.clusterPort}"

    storage "raft" {
      path    = "/var/lib/openbao/data"
      node_id = "${cfg.nodeId}"
    }

    listener "tcp" {
      address         = "${cfg.address}:${toString cfg.port}"
      cluster_address = "127.0.0.1:${toString cfg.clusterPort}"
      tls_cert_file   = "${cfg.tls.certificate}"
      tls_key_file    = "/run/credentials/openbao.service/server.key"
      tls_min_version = "tls13"
    }

    audit "file" "file" {
      options {
        file_path = "/var/log/openbao/audit.log"
      }
    }
  '';
in
{
  options.tabkit.openbao = {
    enable = mkOption {
      type = types.bool;
      default = false;
      description = "Whether the machine runs OpenBao with the settings of this module.";
    };
    package = mkOption {
      type = types.package;
      default = pkgs.openbao;
      defaultText = lib.literalExpression "pkgs.openbao";
      description = "The OpenBao package.";
    };
    address = mkOption {
      type = types.str;
      default = "0.0.0.0";
      description = "The address that the listener binds.";
    };
    port = mkOption {
      type = types.port;
      default = 8200;
      description = "The port of the API.";
    };
    clusterPort = mkOption {
      type = types.port;
      default = 8201;
      description = "The port of the Raft cluster. It listens on the loopback address only.";
    };
    apiAddress = mkOption {
      type = types.str;
      example = "https://10.20.0.30:8200";
      description = "The address that a client uses. Its host name is in the server certificate.";
    };
    nodeId = mkOption {
      type = types.str;
      default = config.networking.hostName;
      defaultText = lib.literalExpression "config.networking.hostName";
      description = "The ID of this node in Raft.";
    };
    clients = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "10.20.0.1/32" ];
      description = "The source ranges that the input firewall opens the API to.";
    };
    tls = {
      certificate = mkOption {
        type = types.path;
        description = "The public certificate of the server.";
      };
      keyFile = mkOption {
        type = types.str;
        example = "/run/secrets/openbao.key";
        description = "The private key of the server, as a path at run time outside the Nix store.";
      };
    };
    snapshot = {
      enable = mkOption {
        type = types.bool;
        default = false;
        description = "Whether a timer writes an encrypted Raft snapshot.";
      };
      schedule = mkOption {
        type = types.str;
        default = "daily";
        description = "The systemd calendar of the snapshot.";
      };
      caFile = mkOption {
        type = types.path;
        description = "The CA certificate that signs the server certificate.";
      };
      clientCertificate = mkOption {
        type = types.path;
        description = "The public client certificate that signs in to the cert auth method.";
      };
      clientKeyFile = mkOption {
        type = types.str;
        example = "/run/secrets/snapshot.key";
        description = "The private key of the client certificate, as a path at run time outside the Nix store.";
      };
      role = mkOption {
        type = types.str;
        default = "snapshot";
        description = "The role of the cert auth method. Its policy reads only sys/storage/raft/snapshot.";
      };
      ageRecipientsFile = mkOption {
        type = types.path;
        description = "A file of age public keys that can decrypt a snapshot. No matching private key lives on the machine.";
      };
      keep = mkOption {
        type = types.ints.positive;
        default = 7;
        description = "The number of encrypted snapshots that the machine keeps.";
      };
      upload = mkOption {
        type = types.lines;
        default = "";
        description = "Shell lines that copy the encrypted file at $SNAPSHOT_FILE to its off-machine store.";
      };
      uploadPackages = mkOption {
        type = types.listOf types.package;
        default = [ ];
        description = "The tools that the upload lines call.";
      };
      environmentFile = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "A file with the variables of the upload, as a path at run time outside the Nix store.";
      };
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        assertions = [
          {
            assertion = !lib.hasPrefix builtins.storeDir cfg.tls.keyFile;
            message = "tabkit.openbao: a secret is a path at run time, outside the Nix store.";
          }
        ];

        environment.systemPackages = [ cfg.package ];

        systemd.services.openbao = {
          description = "OpenBao";
          wantedBy = [ "multi-user.target" ];
          after = [ "network-online.target" ];
          wants = [ "network-online.target" ];
          serviceConfig = {
            ExecStart = "${bao} server -config=${serverConfig}";
            LoadCredential = [ "server.key:${cfg.tls.keyFile}" ];
            DynamicUser = true;
            User = "openbao";
            StateDirectory = "openbao";
            StateDirectoryMode = "0700";
            LogsDirectory = "openbao";
            LogsDirectoryMode = "0700";
            Restart = "on-failure";
            CapabilityBoundingSet = "";
            NoNewPrivileges = true;
            ProtectSystem = "strict";
            PrivateTmp = true;
          };
        };

        networking.firewall.extraInputRules = lib.mkIf (cfg.clients != [ ]) ''
          ip saddr { ${lib.concatStringsSep ", " cfg.clients} } tcp dport ${toString cfg.port} accept
        '';
      }

      (lib.mkIf snapshot.enable {
        assertions = [
          {
            assertion = lib.all (p: p == null || !lib.hasPrefix builtins.storeDir p) [
              snapshot.clientKeyFile
              snapshot.environmentFile
            ];
            message = "tabkit.openbao.snapshot: a secret is a path at run time, outside the Nix store.";
          }
        ];

        systemd.services.tabkit-openbao-snapshot = {
          description = "Write an encrypted OpenBao snapshot";
          after = [ "openbao.service" ];
          path = [
            cfg.package
            pkgs.age
            pkgs.coreutils
            pkgs.findutils
          ]
          ++ snapshot.uploadPackages;
          environment = {
            BAO_ADDR = cfg.apiAddress;
            BAO_CACERT = "${snapshot.caFile}";
            BAO_CLIENT_CERT = "${snapshot.clientCertificate}";
            HOME = "/tmp";
          };
          serviceConfig = {
            Type = "oneshot";
            DynamicUser = true;
            StateDirectory = "openbao-snapshots";
            StateDirectoryMode = "0700";
            LoadCredential = [ "client.key:${snapshot.clientKeyFile}" ];
            NoNewPrivileges = true;
            ProtectSystem = "strict";
            PrivateTmp = true;
            UMask = "0077";
          }
          // lib.optionalAttrs (snapshot.environmentFile != null) {
            EnvironmentFile = snapshot.environmentFile;
          };
          script = ''
            set -euo pipefail
            export BAO_CLIENT_KEY="$CREDENTIALS_DIRECTORY/client.key"
            BAO_TOKEN=$(bao login -method=cert -token-only -no-store name=${lib.escapeShellArg snapshot.role})
            export BAO_TOKEN
            plain=$(mktemp)
            trap 'rm -f "$plain"' EXIT
            bao operator raft snapshot save "$plain"

            dir=/var/lib/openbao-snapshots
            SNAPSHOT_FILE="$dir/snapshot-$(date -u +%Y%m%dT%H%M%SZ).snap.age"
            export SNAPSHOT_FILE
            age -R ${snapshot.ageRecipientsFile} \
              -o "$SNAPSHOT_FILE.tmp" "$plain"
            mv "$SNAPSHOT_FILE.tmp" "$SNAPSHOT_FILE"
            ${snapshot.upload}

            find "$dir" -maxdepth 1 -name 'snapshot-*.snap.age' -printf '%f\n' | sort -r \
              | tail -n +${toString (snapshot.keep + 1)} | while read -r old; do rm -f "$dir/$old"; done
          '';
        };

        systemd.timers.tabkit-openbao-snapshot = {
          wantedBy = [ "timers.target" ];
          timerConfig = {
            OnCalendar = snapshot.schedule;
            Persistent = true;
          };
        };
      })
    ]
  );
}
