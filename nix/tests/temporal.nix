# The nixosTest of the temporal module, per ADR-0300. It proves the schema setup on PostgreSQL 19 over verified TLS,
# that a second run of the setup changes nothing, and that the server serves a namespace.
{
  pkgs,
  module,
  postgres,
}:
let
  tls = pkgs.runCommand "temporal-test-tls" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir $out
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 3650 -subj /CN=db \
      -addext subjectAltName=DNS:db,IP:192.168.1.1 -keyout $out/server.key -out $out/server.crt
  '';
in
pkgs.testers.runNixOSTest {
  name = "temporal";

  nodes.machine =
    { nodes, ... }:
    let
      address = nodes.machine.networking.primaryIPAddress;
    in
    {
      imports = [
        module
        postgres
      ];
      virtualisation = {
        cores = 2;
        memorySize = 2048;
      };
      networking.nftables.enable = true;
      environment.systemPackages = [ pkgs.temporal-cli ];

      systemd.tmpfiles.rules = [
        "d /run/test 0755 root root -"
        "C /run/test/server.key 0400 root root - ${tls}/server.key"
        "f /run/test/roles.sql 0400 root root - CREATE ROLE temporal LOGIN PASSWORD 'a-password';"
        "f /run/test/temporal.env 0400 root root - POSTGRES_PWD=a-password"
      ];

      tabkit.postgres = {
        enable = true;
        package = pkgs.postgresql_19;
        inherit address;
        clients = [ "${address}/32" ];
        tls = {
          certificate = "${tls}/server.crt";
          keyFile = "/run/test/server.key";
        };
        rolesFile = "/run/test/roles.sql";
        setup = ''
          SELECT 'CREATE DATABASE temporal OWNER temporal' WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'temporal')\gexec
          SELECT 'CREATE DATABASE temporal_visibility OWNER temporal' WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'temporal_visibility')\gexec
        '';
      };

      tabkit.temporal = {
        enable = true;
        environmentFile = "/run/test/temporal.env";
        database = {
          host = address;
          caFile = "${tls}/server.crt";
          serverName = "db";
          clientPackage = pkgs.postgresql_19;
        };
      };
    };

  testScript = ''
    temporal = "temporal --address 127.0.0.1:7233"

    machine.wait_for_unit("tabkit-postgres-setup.service")
    machine.wait_for_unit("tabkit-temporal-schema.service", timeout=300)
    machine.wait_for_unit("temporal.service")
    machine.wait_for_open_port(7233, timeout=120)

    with subtest("The server answers on PostgreSQL 19"):
        machine.wait_until_succeeds(f"{temporal} operator cluster health | grep -q SERVING", timeout=120)
        out = machine.succeed("sudo -u postgres psql -tAc 'show server_version_num'")
        assert out.startswith("19"), out

    with subtest("Both schemas exist"):
        for database in ("temporal", "temporal_visibility"):
            machine.succeed(f"sudo -u postgres psql {database} -tAc 'select count(*) from schema_version' | grep -qx 1")

    with subtest("A namespace is created and read"):
        machine.wait_until_succeeds(f"{temporal} operator namespace create --namespace test", timeout=60)
        machine.wait_until_succeeds(f"{temporal} operator namespace describe --namespace test", timeout=60)

    with subtest("A second schema run changes nothing"):
        before = machine.succeed("sudo -u postgres psql temporal -tAc 'select curr_version from schema_version'")
        machine.succeed("systemctl restart tabkit-temporal-schema.service")
        after = machine.succeed("sudo -u postgres psql temporal -tAc 'select curr_version from schema_version'")
        assert before == after, f"{before} != {after}"
        machine.wait_until_succeeds(f"{temporal} operator namespace describe --namespace test", timeout=120)
  '';
}
