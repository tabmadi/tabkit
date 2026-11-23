# The check of ADR-0300 that every NixOS module evaluates on the stable release that machines run. It evaluates one
# machine with each module turned on. It builds nothing of that machine.
{
  pkgs,
  stable,
  modules,
}:
let
  cert = builtins.toFile "test.crt" "public";
  machine = stable.lib.nixosSystem {
    system = pkgs.stdenv.hostPlatform.system;
    modules = modules ++ [
      {
        system.stateVersion = "26.05";
        boot.loader.grub.enable = false;
        fileSystems."/" = {
          device = "/dev/vda";
          fsType = "ext4";
        };
        documentation.enable = false;

        tabkit.server = {
          enable = true;
          adminKeys = [ "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample test" ];
          diskHealth.enable = true;
        };
        tabkit.microvmHost = {
          enable = true;
          gateway = "10.20.0.1";
          uplink = "eth0";
          guests.app = {
            address = "10.20.0.10";
            mac = "02:00:00:00:00:10";
            publish = [ 443 ];
            publishSources = [ "203.0.113.0/24" ];
          };
        };
        tabkit.postgres = {
          enable = true;
          address = "10.20.0.20";
          tls = {
            certificate = cert;
            keyFile = "/run/secrets/postgres.key";
          };
          backup.enable = true;
        };
        tabkit.openbao = {
          enable = true;
          apiAddress = "https://127.0.0.1:8200";
          tls = {
            certificate = cert;
            keyFile = "/run/secrets/openbao.key";
          };
          snapshot = {
            enable = true;
            caFile = cert;
            clientCertificate = cert;
            clientKeyFile = "/run/secrets/snapshot.key";
            ageRecipientsFile = cert;
          };
        };
        tabkit.temporal = {
          enable = true;
          environmentFile = "/run/secrets/temporal.env";
          database = {
            host = "127.0.0.1";
            caFile = cert;
            serverName = "db";
          };
        };
        tabkit.monitoring = {
          enable = true;
          environment = "test";
          mail = {
            smarthost = "127.0.0.1:1025";
            from = "alerts@example.com";
          };
        };
        tabkit.cloudflareOrigin = {
          enable = true;
          domain = "example.com";
          upstream = "http://127.0.0.1:3000";
          certificateFile = "/run/secrets/origin.crt";
          keyFile = "/run/secrets/origin.key";
        };
      }
    ];
  };
in
builtins.seq machine.config.system.build.toplevel.drvPath (
  pkgs.runCommand "module-stable" { } "touch $out"
)
