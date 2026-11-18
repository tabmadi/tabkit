# The nixosTest of the mTLS gate, per ADR-0300. It proves that a request reaches the upstream only
# with a client certificate of the right CA, and only from an allowed host.
{ pkgs, module }:
let
  addresses = {
    gate = "192.168.1.10";
    allowed = "192.168.1.20";
    other = "192.168.1.30";
  };

  # A throwaway PKI for the test only. Its keys are in the Nix store, which is fine for a test.
  # The rogue CA signs a client certificate that the gate must refuse.
  pki = pkgs.runCommand "mtls-gate-test-pki" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir -p $out && cd $out
    ca() {
      openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 30 \
        -subj /CN=$1 -keyout $1.key -out $1.crt
    }
    issue() {
      openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj /CN=$1 -keyout $1.key -out $1.csr
      printf "$3" > $1.ext
      openssl x509 -req -in $1.csr -CA $2.crt -CAkey $2.key -days 30 -out $1.crt -extfile $1.ext
    }
    ca ca
    ca rogue-ca
    issue server ca "subjectAltName=IP:${addresses.gate}\nextendedKeyUsage=serverAuth\n"
    issue client ca "extendedKeyUsage=clientAuth\n"
    issue rogue rogue-ca "extendedKeyUsage=clientAuth\n"
  '';

  # The upstream answers with the client subject that the gate passed on, and logs each request.
  upstream = pkgs.writeText "upstream.py" ''
    from http.server import BaseHTTPRequestHandler, HTTPServer


    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            body = self.headers.get("X-Client-Subject", "none").encode()
            self.send_response(200)
            self.end_headers()
            self.wfile.write(body)


    HTTPServer(("127.0.0.1", 8080), Handler).serve_forever()
  '';

  address = ip: {
    networking.interfaces.eth1.ipv4.addresses = pkgs.lib.mkForce [
      {
        address = ip;
        prefixLength = 24;
      }
    ];
  };

  client = ip: {
    imports = [ (address ip) ];
    environment.systemPackages = [ pkgs.curl ];
  };
in
pkgs.testers.runNixOSTest {
  name = "mtls-gate";

  nodes = {
    gate = {
      imports = [
        module
        (address addresses.gate)
      ];
      networking.nftables.enable = true;
      networking.firewall.enable = true;

      # The key reaches the gate at run time, as a real secret does.
      systemd.tmpfiles.rules = [
        "d /run/gate 0700 root root -"
        "C /run/gate/server.key 0400 root root - ${pki}/server.key"
      ];

      systemd.services.upstream = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 -u ${upstream}";
      };

      tabnix.mtlsGate.test = {
        address = addresses.gate;
        port = 8443;
        upstream = "http://127.0.0.1:8080";
        certificate = "${pki}/server.crt";
        key = "/run/gate/server.key";
        clientCa = "${pki}/ca.crt";
        allowFrom = [ addresses.allowed ];
      };
    };
    allowed = client addresses.allowed;
    other = client addresses.other;
  };

  testScript = ''
    url = "https://${addresses.gate}:8443/"
    curl = "curl -s --cacert ${pki}/ca.crt --connect-timeout 5"
    client = "--cert ${pki}/client.crt --key ${pki}/client.key"
    rogue = "--cert ${pki}/rogue.crt --key ${pki}/rogue.key"

    def upstream_requests():
        return int(gate.succeed("journalctl -u upstream -o cat | grep -c 'GET /' || true").strip())

    start_all()
    gate.wait_for_unit("upstream.service")
    gate.wait_for_unit("nginx.service")
    gate.wait_for_open_port(8443, "${addresses.gate}")
    allowed.wait_for_unit("multi-user.target")
    other.wait_for_unit("multi-user.target")

    with subtest("A client certificate of the CA reaches the upstream"):
        body = allowed.succeed(f"{curl} --fail {client} {url}")
        assert body == "CN=client", body

    with subtest("A client cannot set the identity header itself"):
        body = allowed.succeed(f"{curl} --fail {client} -H 'X-Client-Subject: CN=admin' {url}")
        assert body == "CN=client", body

    with subtest("A request without a client certificate never reaches the upstream"):
        before = upstream_requests()
        code = allowed.succeed(f"{curl} -o /dev/null -w '%{{http_code}}' {url} || true")
        assert code == "400", code
        assert upstream_requests() == before

    with subtest("A client certificate of another CA never reaches the upstream"):
        before = upstream_requests()
        code = allowed.succeed(f"{curl} -o /dev/null -w '%{{http_code}}' {rogue} {url} || true")
        assert code in ("000", "400"), code
        assert upstream_requests() == before

    with subtest("A host that is not allowed never reaches the port"):
        other.fail(f"{curl} {client} {url}")
  '';
}
