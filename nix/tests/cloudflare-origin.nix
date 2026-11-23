# The nixosTest of the cloudflare-origin module, per ADR-0300. It proves that a listed source reaches the upstream over
# TLS, that any other source is refused, and that the upstream sees the address of the peer.
{ pkgs, module }:
let
  tls = pkgs.runCommand "origin-test-tls" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir $out
    openssl req -x509 -newkey ed25519 -nodes -days 3650 -subj /CN=origin.test \
      -addext subjectAltName=DNS:origin.test -keyout $out/origin.key -out $out/origin.crt
  '';

  # It answers with the X-Real-IP header that nginx sets.
  upstream = pkgs.writers.writePython3 "upstream" { flakeIgnore = [ "E501" ]; } ''
    from http.server import BaseHTTPRequestHandler, HTTPServer


    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            body = (self.headers.get("X-Real-IP") or "none").encode()
            self.send_response(200)
            self.end_headers()
            self.wfile.write(body)


    HTTPServer(("127.0.0.1", 3000), Handler).serve_forever()
  '';

  request =
    originAddress:
    "curl -s --connect-timeout 5 --cacert ${tls}/origin.crt --resolve origin.test:443:${originAddress}";
in
pkgs.testers.runNixOSTest {
  name = "cloudflare-origin";

  nodes = {
    allowed = {
      environment.systemPackages = [ pkgs.curl ];
    };
    denied = {
      environment.systemPackages = [ pkgs.curl ];
    };
    origin =
      { nodes, ... }:
      {
        imports = [ module ];
        networking.nftables.enable = true;
        systemd.tmpfiles.rules = [
          "d /run/test 0700 root root -"
          "C /run/test/origin.key 0400 root root - ${tls}/origin.key"
        ];
        systemd.services.upstream = {
          wantedBy = [ "multi-user.target" ];
          serviceConfig.ExecStart = upstream;
        };
        tabkit.cloudflareOrigin = {
          enable = true;
          domain = "origin.test";
          upstream = "http://127.0.0.1:3000";
          certificateFile = "${tls}/origin.crt";
          keyFile = "/run/test/origin.key";
          extraSources = [ "${nodes.allowed.networking.primaryIPAddress}/32" ];
        };
      };
  };

  testScript =
    { nodes, ... }:
    let
      curl = request nodes.origin.networking.primaryIPAddress;
    in
    ''
      start_all()
      origin.wait_for_unit("nginx.service")
      origin.wait_for_unit("upstream.service")
      origin.wait_for_open_port(443)
      origin.wait_for_open_port(3000)

      with subtest("A listed source reaches the upstream over TLS"):
          out = allowed.succeed("${curl} -f https://origin.test/")
          assert out.strip() == "${nodes.allowed.networking.primaryIPAddress}", out

      with subtest("A forged client address does not replace the peer"):
          out = allowed.succeed("${curl} -f -H 'X-Real-IP: 203.0.113.7' https://origin.test/")
          assert out.strip() == "${nodes.allowed.networking.primaryIPAddress}", out

      with subtest("Any other source is refused"):
          code = denied.succeed("${curl} -o /dev/null -w '%{http_code}' https://origin.test/")
          assert code.strip() == "403", code

      with subtest("The upstream is closed from beyond the machine"):
          allowed.fail("curl -sf --connect-timeout 5 http://origin:3000/")
    '';
}
