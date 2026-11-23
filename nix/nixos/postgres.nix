# PostgreSQL for one machine, per ADR-0300: TLS and SCRAM on every network connection, roles from a secret at run
# time, and continuous WAL archiving with a scheduled full backup through pgBackRest.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib) mkOption types;

  cfg = config.tabkit.postgres;
  backup = cfg.backup;
  dataDir = config.services.postgresql.dataDir;
  logPath = "/var/log/pgbackrest";
  lockPath = "/run/pgbackrest";
  pgbackrest = "${backup.package}/bin/pgbackrest";
  command = "${pgbackrest} --stanza=${backup.stanza}";

  repositoryPath =
    if (backup.repository.repo1-type or "posix") == "posix" then
      [ (backup.repository.repo1-path or "/var/lib/pgbackrest") ]
    else
      [ ];

  backupConfig = lib.generators.toINI { } {
    global = {
      repo1-retention-full = toString backup.retentionFull;
      log-path = logPath;
      lock-path = lockPath;
      log-level-console = "info";
      log-level-file = "info";
      start-fast = "y";
    }
    // backup.repository;
    ${backup.stanza} = {
      pg1-path = dataDir;
      pg1-socket-path = "/run/postgresql";
    };
  };

  # A unit of pgBackRest reads the credentials of the repository, and only those.
  backupService = {
    after = [ "postgresql.service" ];
    requires = [ "postgresql.service" ];
    serviceConfig = {
      Type = "oneshot";
      User = "postgres";
      Group = "postgres";
    }
    // lib.optionalAttrs (backup.environmentFile != null) {
      EnvironmentFile = backup.environmentFile;
    };
  };
in
{
  options.tabkit.postgres = {
    enable = mkOption {
      type = types.bool;
      default = false;
      description = "Whether the machine runs PostgreSQL with the settings of this module.";
    };
    package = mkOption {
      type = types.package;
      default = pkgs.postgresql;
      defaultText = lib.literalExpression "pkgs.postgresql";
      description = "The PostgreSQL package.";
    };
    address = mkOption {
      type = types.str;
      example = "10.20.0.20";
      description = "The address that PostgreSQL listens on, beside the loopback address.";
    };
    clients = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "10.20.0.10/32" ];
      description = "The source ranges that may connect over TLS with a SCRAM password.";
    };
    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = "Whether the input firewall opens the port to the clients only.";
    };
    tls = {
      certificate = mkOption {
        type = types.path;
        description = "The public certificate of the server.";
      };
      keyFile = mkOption {
        type = types.str;
        example = "/run/secrets/postgres.key";
        description = "The private key of the server, as a path at run time outside the Nix store.";
      };
    };
    rolesFile = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/run/secrets/roles.sql";
      description = "A SQL file that creates the login roles, as a path at run time outside the Nix store.";
    };
    setup = mkOption {
      type = types.lines;
      default = "";
      description = "psql input that creates the databases and grants. It runs on each boot, so it changes only what is missing.";
    };
    backup = {
      enable = mkOption {
        type = types.bool;
        default = false;
        description = "Whether pgBackRest archives the WAL and takes a scheduled full backup.";
      };
      package = mkOption {
        type = types.package;
        default = pkgs.pgbackrest;
        defaultText = lib.literalExpression "pkgs.pgbackrest";
        description = "The pgBackRest package.";
      };
      stanza = mkOption {
        type = types.strMatching "[a-z0-9-]+";
        default = "main";
        description = "The name of the pgBackRest stanza.";
      };
      repository = mkOption {
        type = types.attrsOf types.str;
        default = {
          repo1-type = "posix";
          repo1-path = "/var/lib/pgbackrest";
        };
        example = {
          repo1-type = "s3";
          repo1-path = "/postgres";
          repo1-s3-bucket = "backups";
          repo1-s3-endpoint = "s3.example.com";
          repo1-s3-region = "auto";
          repo1-cipher-type = "aes-256-cbc";
        };
        description = "The repo1 settings of pgBackRest. A secret goes in environmentFile, never here.";
      };
      environmentFile = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "/run/secrets/pgbackrest.env";
        description = "A file with the PGBACKREST_ variables of the repository secrets, as a path at run time.";
      };
      retentionFull = mkOption {
        type = types.ints.positive;
        default = 7;
        description = "The number of full backups that the repository keeps.";
      };
      schedule = mkOption {
        type = types.str;
        default = "daily";
        description = "The systemd calendar of the full backup.";
      };
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        assertions = [
          {
            assertion = lib.all (p: p == null || !lib.hasPrefix builtins.storeDir p) [
              cfg.tls.keyFile
              cfg.rolesFile
              backup.environmentFile
            ];
            message = "tabkit.postgres: a secret is a path at run time, outside the Nix store.";
          }
          {
            assertion = !(backup.repository ? repo1-s3-key || backup.repository ? repo1-s3-key-secret);
            message = "tabkit.postgres.backup.repository: an S3 key goes in environmentFile.";
          }
        ];

        services.postgresql = {
          enable = true;
          inherit (cfg) package;
          enableTCPIP = true;
          settings = {
            listen_addresses = lib.mkForce "localhost,${cfg.address}";
            ssl = true;
            ssl_cert_file = "${cfg.tls.certificate}";
            ssl_key_file = "/run/credentials/postgresql.service/server.key";
            ssl_min_protocol_version = "TLSv1.3";
            password_encryption = "scram-sha-256";
            wal_level = "replica";
            max_wal_senders = lib.mkDefault 3;
          };
          authentication = lib.mkForce (
            ''
              local all postgres peer
              local all all scram-sha-256
              host all all 127.0.0.1/32 scram-sha-256
            ''
            + lib.concatMapStrings (client: "hostssl all all ${client} scram-sha-256\n") cfg.clients
          );
        };

        # PostgreSQL reads its key as its own user, so systemd hands over a copy.
        systemd.services.postgresql.serviceConfig.LoadCredential = [ "server.key:${cfg.tls.keyFile}" ];

        systemd.services.tabkit-postgres-setup = {
          description = "Create the roles, the databases, and the grants";
          wantedBy = [ "multi-user.target" ];
          requires = [ "postgresql.service" ];
          after = [ "postgresql.service" ];
          path = [ cfg.package ];
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
            User = "postgres";
          }
          // lib.optionalAttrs (cfg.rolesFile != null) {
            LoadCredential = [ "roles.sql:${cfg.rolesFile}" ];
          };
          script = ''
            ${lib.optionalString (
              cfg.rolesFile != null
            ) ''psql -v ON_ERROR_STOP=1 -q -f "$CREDENTIALS_DIRECTORY/roles.sql"''}
            psql -v ON_ERROR_STOP=1 -q -f ${pkgs.writeText "postgres-setup.sql" cfg.setup}
          '';
        };

        networking.firewall.extraInputRules = lib.mkIf (cfg.openFirewall && cfg.clients != [ ]) ''
          ip saddr { ${lib.concatStringsSep ", " cfg.clients} } tcp dport ${toString config.services.postgresql.settings.port} accept
        '';
      }

      (lib.mkIf backup.enable {
        environment.systemPackages = [ backup.package ];
        environment.etc."pgbackrest/pgbackrest.conf".text = backupConfig;

        systemd.tmpfiles.rules = [
          "d ${logPath} 0750 postgres postgres -"
          "d ${lockPath} 0750 postgres postgres -"
        ]
        ++ map (path: "d ${path} 0750 postgres postgres -") repositoryPath;

        services.postgresql.settings = {
          archive_mode = "on";
          archive_command = "${command} archive-push %p";
        };

        # The archive command runs inside the sandbox of PostgreSQL.
        systemd.services.postgresql.serviceConfig = {
          ReadWritePaths = [
            logPath
            lockPath
          ]
          ++ repositoryPath;
        }
        // lib.optionalAttrs (backup.environmentFile != null) {
          EnvironmentFile = backup.environmentFile;
        };

        systemd.services.tabkit-postgres-stanza = lib.recursiveUpdate backupService {
          description = "Create the pgBackRest stanza";
          wantedBy = [ "multi-user.target" ];
          serviceConfig.RemainAfterExit = true;
          script = ''
            ${command} stanza-create
            ${command} check
          '';
        };

        systemd.services.tabkit-postgres-backup = lib.recursiveUpdate backupService {
          description = "Take a full pgBackRest backup";
          after = [ "tabkit-postgres-stanza.service" ];
          requires = [ "tabkit-postgres-stanza.service" ];
          script = "${command} --type=full backup";
        };

        systemd.timers.tabkit-postgres-backup = {
          wantedBy = [ "timers.target" ];
          timerConfig = {
            OnCalendar = backup.schedule;
            Persistent = true;
          };
        };
      })
    ]
  );
}
