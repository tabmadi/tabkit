# The nixosTest of the postgres module, per ADR-0300. It proves the refusals of TLS and SCRAM, the roles from a secret,
# and a pgBackRest backup and restore on PostgreSQL 19, with a row that only the archived WAL holds.
{ pkgs, module }:
let
  tls = pkgs.runCommand "postgres-test-tls" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir $out
    openssl req -x509 -newkey ed25519 -nodes -days 3650 -subj /CN=server \
      -addext subjectAltName=DNS:server -keyout $out/server.key -out $out/server.crt
  '';
  psql = "${pkgs.postgresql_19}/bin/psql";
in
pkgs.testers.runNixOSTest {
  name = "postgres";

  nodes = {
    server =
      { nodes, ... }:
      {
        imports = [ module ];
        virtualisation.memorySize = 1536;
        networking.nftables.enable = true;
        environment.systemPackages = [ pkgs.postgresql_19 ];

        # Each secret reaches the machine at run time, as a real secret does.
        systemd.tmpfiles.rules = [
          "d /run/test 0700 root root -"
          "C /run/test/server.key 0400 root root - ${tls}/server.key"
          "f /run/test/roles.sql 0400 root root - CREATE ROLE app LOGIN PASSWORD 'a-password';"
        ];

        tabkit.postgres = {
          enable = true;
          package = pkgs.postgresql_19;
          address = nodes.server.networking.primaryIPAddress;
          clients = [ "${nodes.client.networking.primaryIPAddress}/32" ];
          openFirewall = true;
          tls = {
            certificate = "${tls}/server.crt";
            keyFile = "/run/test/server.key";
          };
          rolesFile = "/run/test/roles.sql";
          setup = ''
            SELECT 'CREATE DATABASE app OWNER app' WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'app')\gexec
          '';
          backup.enable = true;
        };
      };

    client = { };
  };

  testScript = ''
    url = "host=server dbname=app user=app"
    verified = f"{url} sslmode=verify-full sslrootcert=${tls}/server.crt"

    def remote(sql, conninfo=verified, password="a-password"):
        return f"PGPASSWORD={password} ${psql} '{conninfo}' -tAc {sql!r}"

    def local(sql):
        return f"sudo -u postgres psql app -tAc {sql!r}"

    start_all()
    server.wait_for_unit("tabkit-postgres-setup.service")
    server.wait_for_unit("tabkit-postgres-stanza.service")
    client.wait_for_unit("multi-user.target")

    with subtest("A client connects over TLS 1.3 with its password"):
        out = client.succeed(remote("select version from pg_stat_ssl where pid = pg_backend_pid()"))
        assert out.strip() == "TLSv1.3", out

    with subtest("A connection without TLS is refused"):
        client.fail(remote("select 1", f"{url} sslmode=disable"))

    with subtest("A wrong password is refused"):
        client.fail(remote("select 1", password="wrong"))

    with subtest("A role is stored as SCRAM"):
        out = server.succeed("sudo -u postgres psql -tAc \"select rolpassword from pg_authid where rolname = 'app'\"")
        assert out.startswith("SCRAM-SHA-256$"), out

    with subtest("A backup and the archived WAL restore every row"):
        server.succeed(local("create table rows (value text)"))
        server.succeed(local("insert into rows values ('in-backup')"))
        server.succeed("systemctl start tabkit-postgres-backup.service")
        server.succeed(local("insert into rows values ('in-wal')"))
        server.succeed(local("select pg_switch_wal()"))
        server.wait_until_succeeds(
            "test \"$(sudo -u postgres psql -tAc 'select last_archived_wal is not null and last_failed_time is null from pg_stat_archiver')\" = t"
        )
        server.succeed("sudo -u postgres pgbackrest --stanza=main check")
        server.succeed("systemctl stop postgresql.service")
        data = server.succeed("ls -d /var/lib/postgresql/*/").strip()
        server.succeed(f"find {data} -mindepth 1 -delete")
        server.succeed("sudo -u postgres pgbackrest --stanza=main restore")
        server.succeed("systemctl start postgresql.service")
        server.wait_until_succeeds("sudo -u postgres psql -tAc 'select not pg_is_in_recovery()' | grep -qx t")
        out = server.succeed(local("select string_agg(value, ',' order by value) from rows"))
        assert out.strip() == "in-backup,in-wal", out
  '';
}
