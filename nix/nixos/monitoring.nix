# Prometheus and Alertmanager for one machine, per ADR-0300, with the base alerts of a server. Both listen on the
# loopback address only. A repo adds its own scrape jobs and rule files.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib) mkOption types;

  cfg = config.tabkit.monitoring;

  baseRules = pkgs.writeText "tabkit-base-alerts.yml" (
    builtins.toJSON {
      groups = [
        {
          name = "tabkit-base";
          rules = [
            {
              alert = "TargetDown";
              expr = "up == 0";
              "for" = cfg.targetDownAfter;
              labels.severity = "critical";
              annotations = {
                summary = "{{ $labels.job }} on {{ $labels.instance }} is down.";
                runbook = "${cfg.runbookBase}/host-down.md";
              };
            }
            {
              alert = "DiskAlmostFull";
              expr = ''node_filesystem_avail_bytes{fstype!~"tmpfs|ramfs|squashfs|overlay"} / node_filesystem_size_bytes{fstype!~"tmpfs|ramfs|squashfs|overlay"} < 0.1'';
              "for" = "10m";
              labels.severity = "warning";
              annotations = {
                summary = "{{ $labels.mountpoint }} on {{ $labels.instance }} has less than 10 percent free.";
                runbook = "${cfg.runbookBase}/disk-full.md";
              };
            }
            {
              alert = "RaidDegraded";
              expr = ''node_md_disks{state="failed"} > 0 or node_md_disks_required - ignoring(state) node_md_disks{state="active"} > 0'';
              "for" = "5m";
              labels.severity = "critical";
              annotations = {
                summary = "The array {{ $labels.device }} on {{ $labels.instance }} has lost a disk.";
                runbook = "${cfg.runbookBase}/disk-failed.md";
              };
            }
            {
              alert = "DiskSmartFailing";
              expr = "smartctl_device_smart_status == 0";
              "for" = "5m";
              labels.severity = "critical";
              annotations = {
                summary = "The disk {{ $labels.device }} on {{ $labels.instance }} reports a SMART failure.";
                runbook = "${cfg.runbookBase}/disk-failed.md";
              };
            }
          ];
        }
      ];
    }
  );
in
{
  options.tabkit.monitoring = {
    enable = mkOption {
      type = types.bool;
      default = false;
      description = "Whether the machine runs Prometheus and Alertmanager.";
    };
    environment = mkOption {
      type = types.str;
      example = "dev";
      description = "The value of the `env` label on every metric and alert.";
    };
    retention = mkOption {
      type = types.str;
      default = "30d";
      description = "How long Prometheus keeps its data.";
    };
    scrapeInterval = mkOption {
      type = types.str;
      default = "30s";
      description = "The scrape and rule evaluation interval.";
    };
    targetDownAfter = mkOption {
      type = types.str;
      default = "5m";
      description = "How long a target is down before its alert fires.";
    };
    runbookBase = mkOption {
      type = types.str;
      default = "docs/guide/runbooks";
      description = "The folder of the runbooks that the base alerts name.";
    };
    scrapeConfigs = mkOption {
      type = types.listOf types.attrs;
      default = [ ];
      description = "The scrape jobs, in the format of services.prometheus.scrapeConfigs.";
    };
    ruleFiles = mkOption {
      type = types.listOf types.path;
      default = [ ];
      description = "The alert rule files of the repo, beside the base alerts.";
    };
    credentials = mkOption {
      type = types.attrsOf types.str;
      default = { };
      example = {
        metrics-token = "/run/secrets/metrics-token";
      };
      description = "The files that a scrape job reads from /run/credentials/prometheus.service, by name. Each is a path at run time.";
    };
    mail = {
      smarthost = mkOption {
        type = types.str;
        example = "127.0.0.1:1025";
        description = "The SMTP server that Alertmanager sends through.";
      };
      from = mkOption {
        type = types.str;
        example = "alerts@example.com";
        description = "The sender of an alert mail.";
      };
      to = mkOption {
        type = types.str;
        default = "$ALERT_EMAIL_TO";
        description = "The receiver of an alert mail. The default reads it from environmentFile.";
      };
      requireTls = mkOption {
        type = types.bool;
        default = true;
        description = "Whether Alertmanager refuses an SMTP server without STARTTLS.";
      };
      authUsername = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "The SMTP login of Alertmanager. Null sends without a login.";
      };
      authPasswordFile = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "/run/secrets/smtp-password";
        description = "A file with the SMTP password, as a path at run time outside the Nix store.";
      };
    };
    environmentFile = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/run/secrets/alerts.env";
      description = "A file with the variables of Alertmanager, as a path at run time outside the Nix store.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = lib.all (p: p == null || !lib.hasPrefix builtins.storeDir p) (
          [
            cfg.environmentFile
            cfg.mail.authPasswordFile
          ]
          ++ builtins.attrValues cfg.credentials
        );
        message = "tabkit.monitoring: a secret is a path at run time, outside the Nix store.";
      }
    ];

    services.prometheus = {
      enable = true;
      listenAddress = "127.0.0.1";
      retentionTime = cfg.retention;
      # A credential exists only at run time, so the build checks the syntax alone.
      checkConfig = "syntax-only";
      globalConfig = {
        scrape_interval = cfg.scrapeInterval;
        evaluation_interval = cfg.scrapeInterval;
        external_labels.env = cfg.environment;
      };
      ruleFiles = [ baseRules ] ++ cfg.ruleFiles;
      alertmanagers = [ { static_configs = [ { targets = [ "127.0.0.1:9093" ]; } ]; } ];
      inherit (cfg) scrapeConfigs;
      alertmanager = {
        enable = true;
        listenAddress = "127.0.0.1";
        inherit (cfg) environmentFile;
        configuration = {
          global = {
            smtp_smarthost = cfg.mail.smarthost;
            smtp_from = cfg.mail.from;
            smtp_require_tls = cfg.mail.requireTls;
          }
          // lib.optionalAttrs (cfg.mail.authUsername != null) {
            smtp_auth_username = cfg.mail.authUsername;
          }
          // lib.optionalAttrs (cfg.mail.authPasswordFile != null) {
            smtp_auth_password_file = "/run/credentials/alertmanager.service/smtp-password";
          };
          route = {
            receiver = "operators";
            group_by = [
              "alertname"
              "instance"
            ];
            group_wait = "30s";
            group_interval = "5m";
            repeat_interval = "4h";
          };
          receivers = [
            {
              name = "operators";
              email_configs = [
                {
                  inherit (cfg.mail) to;
                  send_resolved = true;
                }
              ];
            }
          ];
        };
      };
    };

    # Alertmanager reads the password as its own user, so systemd hands over a copy.
    systemd.services.alertmanager.serviceConfig.LoadCredential = lib.mkIf (
      cfg.mail.authPasswordFile != null
    ) [ "smtp-password:${cfg.mail.authPasswordFile}" ];

    systemd.services.prometheus.serviceConfig.LoadCredential = lib.mapAttrsToList (
      name: path: "${name}:${path}"
    ) cfg.credentials;
  };
}
