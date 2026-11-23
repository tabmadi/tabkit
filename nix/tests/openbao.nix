# The nixosTest of the openbao module, per ADR-0300. It proves the TLS refusal, the audit log, the narrow snapshot
# policy, that a snapshot on disk holds no plain data, and that a restore brings back a deleted secret.
{ pkgs, module }:
let
  material =
    pkgs.runCommand "openbao-test-material"
      {
        nativeBuildInputs = [
          pkgs.age
          pkgs.openssl
        ];
      }
      ''
        mkdir $out
        cd $out
        openssl req -x509 -newkey ed25519 -nodes -days 3650 -subj /CN=test-ca -keyout ca.key -out ca.crt
        sign() {
          openssl req -newkey ed25519 -nodes -subj "/CN=$1" -keyout "$1.key" -out "$1.csr"
          printf '%s\n' "$2" > "$1.ext"
          openssl x509 -req -in "$1.csr" -CA ca.crt -CAkey ca.key -CAcreateserial -days 3650 \
            -extfile "$1.ext" -out "$1.crt"
        }
        sign server 'subjectAltName=DNS:server,DNS:localhost'
        sign snapshot 'extendedKeyUsage=clientAuth'
        age-keygen -o age.key 2> /dev/null
        age-keygen -y age.key > age.pub
      '';
in
pkgs.testers.runNixOSTest {
  name = "openbao";

  nodes = {
    server = {
      imports = [ module ];
      virtualisation.memorySize = 1536;
      networking.nftables.enable = true;
      environment.systemPackages = [
        pkgs.age
        pkgs.jq
      ];
      networking.hosts."127.0.0.1" = [ "server" ];

      systemd.tmpfiles.rules = [
        "d /run/test 0700 root root -"
        "C /run/test/server.key 0400 root root - ${material}/server.key"
        "C /run/test/snapshot.key 0400 root root - ${material}/snapshot.key"
      ];

      tabkit.openbao = {
        enable = true;
        apiAddress = "https://server:8200";
        tls = {
          certificate = "${material}/server.crt";
          keyFile = "/run/test/server.key";
        };
        clients = [ "192.168.1.0/24" ];
        snapshot = {
          enable = true;
          caFile = "${material}/ca.crt";
          clientCertificate = "${material}/snapshot.crt";
          clientKeyFile = "/run/test/snapshot.key";
          ageRecipients = [ (pkgs.lib.fileContents "${material}/age.pub") ];
          keep = 2;
        };
      };
    };

    client = {
      environment.systemPackages = [ pkgs.curl ];
    };
  };

  testScript = ''
    import json

    env = "BAO_ADDR=https://server:8200 BAO_CACERT=${material}/ca.crt"

    def bao(command, token=None):
        auth = f"BAO_TOKEN={token} " if token else ""
        return f"{auth}{env} bao {command}"

    start_all()
    server.wait_for_unit("openbao.service")
    server.wait_for_open_port(8200)
    client.wait_for_unit("multi-user.target")

    with subtest("Plain HTTP is refused"):
        client.fail("curl -sf --connect-timeout 5 http://server:8200/v1/sys/health")

    with subtest("TLS below version 1.3 is refused"):
        client.fail("curl -sk --connect-timeout 5 --tls-max 1.2 https://server:8200/v1/sys/seal-status")
        client.succeed("curl -s --cacert ${material}/ca.crt --tlsv1.3 https://server:8200/v1/sys/seal-status")

    init = json.loads(server.succeed(bao("operator init -key-shares=1 -key-threshold=1 -format=json")))
    unseal = init["unseal_keys_b64"][0]
    root = init["root_token"]
    server.succeed(bao(f"operator unseal {unseal}"))
    server.wait_until_succeeds(bao("status -format=json") + " | jq -e '.sealed == false and .is_self == true'")

    server.succeed(bao("secrets enable -path=kv kv", root))
    server.succeed(bao("kv put kv/app value=kept-secret-value", root))
    server.succeed(bao("auth enable cert", root))
    server.succeed(
        "printf 'path \"sys/storage/raft/snapshot\" {\\n  capabilities = [\"read\"]\\n}\\n' | "
        + bao("policy write snapshot -", root)
    )
    server.succeed(
        bao("write auth/cert/certs/snapshot display_name=snapshot policies=snapshot "
            "certificate=@${material}/snapshot.crt token_ttl=5m", root)
    )

    with subtest("The audit log records a request and hides the secret"):
        server.succeed("grep -q kv/app /var/log/openbao/audit.log")
        server.fail("grep -q kept-secret-value /var/log/openbao/audit.log")

    with subtest("The snapshot certificate cannot read a secret"):
        token = server.succeed(
            f"{env} BAO_CLIENT_CERT=${material}/snapshot.crt BAO_CLIENT_KEY=/run/test/snapshot.key "
            "bao login -method=cert -token-only -no-store name=snapshot"
        ).strip()
        server.fail(bao("kv get kv/app", token))

    with subtest("A snapshot on disk is encrypted"):
        server.succeed("systemctl start tabkit-openbao-snapshot.service")
        file = server.succeed("ls /var/lib/private/openbao-snapshots/*.snap.age").strip()
        server.succeed(f"head -c 21 {file} | grep -q age-encryption.org")
        server.fail(f"grep -aq kv/ {file}")

    with subtest("The job keeps only the newest snapshots"):
        for _ in range(2):
            server.succeed("sleep 1.1; systemctl start tabkit-openbao-snapshot.service")
        count = server.succeed("ls /var/lib/private/openbao-snapshots/*.snap.age | wc -l").strip()
        assert count == "2", count

    with subtest("A restore brings back a deleted secret"):
        file = server.succeed("ls -t /var/lib/private/openbao-snapshots/*.snap.age | head -1").strip()
        server.succeed(f"age -d -i ${material}/age.key -o /run/test/restore.snap {file}")
        server.succeed(bao("kv delete kv/app", root))
        server.fail(bao("kv get kv/app", root))
        server.succeed(bao("operator raft snapshot restore -force /run/test/restore.snap", root))
        server.wait_until_succeeds(bao("status -format=json") + " | jq -e '.sealed == false and .is_self == true'")
        out = server.wait_until_succeeds(bao("kv get -field=value kv/app", root))
        assert out.strip() == "kept-secret-value", out
  '';
}
