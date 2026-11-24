# The nixosTest of the monitoring module, per ADR-0300. It proves that both services refuse a remote client, that a
# stopped target fires the base alert, and that the alert reaches the mail server.
{ pkgs, module }:
pkgs.testers.runNixOSTest {
  name = "monitoring";

  nodes = {
    server = {
      imports = [ module ];
      virtualisation.memorySize = 1024;
      networking.nftables.enable = true;
      networking.firewall.allowedTCPPorts = [
        9090
        9093
      ];
      environment.systemPackages = [
        pkgs.curl
        pkgs.jq
      ];

      services.prometheus.exporters.node = {
        enable = true;
        listenAddress = "127.0.0.1";
      };
      services.mailpit.instances.test = {
        listen = "127.0.0.1:8025";
        smtp = "127.0.0.1:1025";
      };
      systemd.tmpfiles.rules = [
        "d /run/test 0755 root root -"
        "f /run/test/alerts.env 0444 root root - ALERT_EMAIL_TO=operator@example.com"
      ];

      tabkit.monitoring = {
        enable = true;
        environment = "test";
        scrapeInterval = "2s";
        targetDownAfter = "4s";
        environmentFile = "/run/test/alerts.env";
        mail = {
          smarthost = "127.0.0.1:1025";
          from = "alerts@example.com";
          requireTls = false;
        };
        scrapeConfigs = [
          {
            job_name = "node";
            static_configs = [ { targets = [ "127.0.0.1:9100" ]; } ];
          }
        ];
      };
    };

    client = {
      environment.systemPackages = [ pkgs.curl ];
    };
  };

  testScript = ''
    start_all()
    server.wait_for_unit("prometheus.service")
    server.wait_for_unit("alertmanager.service")
    server.wait_for_unit("prometheus-node-exporter.service")
    server.wait_for_open_port(9090)
    server.wait_for_open_port(9093)
    client.wait_for_unit("multi-user.target")

    with subtest("A remote client cannot reach either service, even with the firewall open"):
        client.fail("curl -sf --connect-timeout 5 http://server:9090/-/healthy")
        client.fail("curl -sf --connect-timeout 5 http://server:9093/-/healthy")

    with subtest("The target is up, with the environment label"):
        server.wait_until_succeeds(
            "curl -gsf 'http://127.0.0.1:9090/api/v1/query?query=up{job=\"node\"}' | jq -e '.data.result[0].value[1] == \"1\"'",
            timeout=60,
        )
        server.succeed("curl -sf http://127.0.0.1:9090/api/v1/status/config | grep -q 'env: test'")

    with subtest("A stopped target fires the alert"):
        server.succeed("systemctl stop prometheus-node-exporter.service")
        server.wait_until_succeeds(
            "curl -sf http://127.0.0.1:9093/api/v2/alerts | jq -e 'any(.labels.alertname == \"TargetDown\")'",
            timeout=120,
        )

    with subtest("The alert reaches the mail server"):
        server.wait_until_succeeds(
            "curl -sf http://127.0.0.1:8025/api/v1/messages | jq -e 'any(.messages[]; .To[0].Address == \"operator@example.com\" and (.Subject | contains(\"TargetDown\")))'",
            timeout=120,
        )
  '';
}
