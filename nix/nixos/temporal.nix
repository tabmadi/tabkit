# The Temporal server on PostgreSQL, per ADR-0300. A oneshot unit creates or upgrades both schemas before the server
# starts. Every database connection verifies TLS, and the password reaches the server only at run time.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib) mkOption types;

  cfg = config.tabkit.temporal;
  db = cfg.database;
  temporal = cfg.package;

  datastore = name: maxConns: ''
    ${name}:
      sql:
        pluginName: postgres12
        databaseName: ${if name == "default" then db.name else db.visibilityName}
        connectAddr: "${db.host}:${toString db.port}"
        connectProtocol: tcp
        user: ${db.user}
        password: {{ env "POSTGRES_PWD" | quote }}
        maxConns: ${toString maxConns}
        maxIdleConns: ${toString maxConns}
        maxConnLifetime: 1h
        tls:
          enabled: true
          caFile: ${db.caFile}
          enableHostVerification: true
          serverName: ${db.serverName}
  '';

  serverConfig = pkgs.writeText "temporal.yaml" ''
    # enable-template
    log:
      stdout: true
      level: info
    persistence:
      defaultStore: default
      visibilityStore: visibility
      numHistoryShards: ${toString cfg.historyShards}
      datastores:
    ${lib.concatMapStrings (line: "    ${line}\n") (
      lib.splitString "\n" (lib.removeSuffix "\n" (datastore "default" 20 + datastore "visibility" 10))
    )}
    global:
      membership:
        maxJoinDuration: 30s
        broadcastAddress: 127.0.0.1
      metrics:
        prometheus:
          timerType: histogram
          listenAddress: ${cfg.metricsAddress}
    services:
      frontend:
        rpc: { grpcPort: ${toString cfg.port}, membershipPort: 6933, bindOnIP: 127.0.0.1, httpPort: 7243 }
      matching:
        rpc: { grpcPort: 7235, membershipPort: 6935, bindOnIP: 127.0.0.1 }
      history:
        rpc: { grpcPort: 7234, membershipPort: 6934, bindOnIP: 127.0.0.1 }
      worker:
        rpc: { grpcPort: 7239, membershipPort: 6939, bindOnIP: 127.0.0.1 }
    clusterMetadata:
      enableGlobalNamespace: false
      failoverVersionIncrement: 10
      masterClusterName: active
      currentClusterName: active
      clusterInformation:
        active:
          enabled: true
          initialFailoverVersion: 1
          rpcName: frontend
          rpcAddress: 127.0.0.1:${toString cfg.port}
          httpAddress: 127.0.0.1:7243
    dcRedirectionPolicy:
      policy: noop
    archival:
      history: { state: disabled }
      visibility: { state: disabled }
    ${lib.optionalString (cfg.dynamicConfig != null) ''
      dynamicConfigClient:
        filepath: ${cfg.dynamicConfig}
        pollInterval: 60s
    ''}
  '';
in
{
  options.tabkit.temporal = {
    enable = mkOption {
      type = types.bool;
      default = false;
      description = "Whether the machine runs the Temporal server.";
    };
    package = mkOption {
      type = types.package;
      default = pkgs.temporal;
      defaultText = lib.literalExpression "pkgs.temporal";
      description = "The Temporal server package, with temporal-sql-tool and the schema files.";
    };
    port = mkOption {
      type = types.port;
      default = 7233;
      description = "The gRPC port of the frontend. It listens on the loopback address only.";
    };
    metricsAddress = mkOption {
      type = types.str;
      default = "127.0.0.1:9095";
      description = "The address of the Prometheus metrics.";
    };
    historyShards = mkOption {
      type = types.ints.positive;
      default = 4;
      description = "The number of history shards. A running cluster cannot change it.";
    };
    dynamicConfig = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = "The dynamic config file of the server.";
    };
    environmentFile = mkOption {
      type = types.str;
      example = "/run/secrets/temporal.env";
      description = "A file that sets POSTGRES_PWD, as a path at run time outside the Nix store.";
    };
    database = {
      host = mkOption {
        type = types.str;
        example = "10.20.0.20";
        description = "The address of PostgreSQL.";
      };
      port = mkOption {
        type = types.port;
        default = 5432;
        description = "The port of PostgreSQL.";
      };
      user = mkOption {
        type = types.str;
        default = "temporal";
        description = "The role that owns both databases.";
      };
      name = mkOption {
        type = types.str;
        default = "temporal";
        description = "The database of the workflow state.";
      };
      visibilityName = mkOption {
        type = types.str;
        default = "temporal_visibility";
        description = "The database of the visibility store.";
      };
      caFile = mkOption {
        type = types.path;
        description = "The CA certificate that signs the PostgreSQL server certificate.";
      };
      serverName = mkOption {
        type = types.str;
        example = "db";
        description = "The name in the PostgreSQL server certificate.";
      };
      clientPackage = mkOption {
        type = types.package;
        default = pkgs.postgresql;
        defaultText = lib.literalExpression "pkgs.postgresql";
        description = "The package of psql, which checks whether a schema exists.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = !lib.hasPrefix builtins.storeDir cfg.environmentFile;
        message = "tabkit.temporal: a secret is a path at run time, outside the Nix store.";
      }
    ];

    environment.systemPackages = [ temporal ];

    # It retries until the database machine has created the databases.
    systemd.services.tabkit-temporal-schema = {
      description = "Create or upgrade the Temporal schema";
      requiredBy = [ "temporal.service" ];
      before = [ "temporal.service" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      path = [
        temporal
        db.clientPackage
      ];
      environment = {
        SQL_PLUGIN = "postgres12";
        SQL_HOST = db.host;
        SQL_PORT = toString db.port;
        SQL_USER = db.user;
        SQL_TLS = "true";
        SQL_TLS_CA_FILE = "${db.caFile}";
        SQL_TLS_SERVER_NAME = db.serverName;
        # A static Go build finds the current user only through $HOME.
        HOME = "/tmp";
      };
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        DynamicUser = true;
        PrivateTmp = true;
        EnvironmentFile = cfg.environmentFile;
        Restart = "on-failure";
        RestartSec = 10;
      };
      script = ''
        export SQL_PASSWORD=$POSTGRES_PWD PGPASSWORD=$POSTGRES_PWD
        for database in ${db.name} ${db.visibilityName}; do
          exists=$(psql "host=${db.serverName} hostaddr=${db.host} port=${toString db.port} dbname=$database user=${db.user} sslmode=verify-full sslrootcert=${db.caFile}" \
            -tAc "select to_regclass('public.schema_version') is not null")
          if [ "$exists" != t ]; then
            temporal-sql-tool --db "$database" setup-schema -v 0.0
          fi
        done
        temporal-sql-tool --db ${db.name} update-schema -d ${temporal}/share/schema/postgresql/v12/temporal/versioned
        temporal-sql-tool --db ${db.visibilityName} update-schema -d ${temporal}/share/schema/postgresql/v12/visibility/versioned
      '';
    };

    systemd.services.temporal = {
      description = "Temporal server";
      wantedBy = [ "multi-user.target" ];
      # A static Go build finds the current user only through $HOME.
      environment.HOME = "/var/lib/temporal";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        ExecStart = "${temporal}/bin/temporal-server --config-file ${serverConfig} --allow-no-auth start";
        EnvironmentFile = cfg.environmentFile;
        DynamicUser = true;
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        PrivateTmp = true;
        StateDirectory = "temporal";
        WorkingDirectory = "/var/lib/temporal";
        Restart = "always";
        RestartSec = 5;
      };
    };
  };
}
