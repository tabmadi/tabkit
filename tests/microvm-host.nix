# The nixosTest of the microVM host, per ADR-0500. A host runs two guests as nested microVMs. The test proves
# that only a declared flow crosses the host, that only a guest with egress reaches the network beyond it, and
# that a guest reads its secrets and cannot change them.
{ pkgs, module }:
let
  keys = import "${pkgs.path}/nixos/tests/ssh-keys.nix" pkgs;

  # A guest that answers HTTP on the given ports with its own name.
  web = name: ports: {
    environment.systemPackages = [ pkgs.curl ];
    systemd.services = builtins.listToAttrs (
      map (port: {
        name = "web-${toString port}";
        value = {
          wantedBy = [ "multi-user.target" ];
          serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 -m http.server ${toString port} --directory /etc/web";
        };
      }) ports
    );
    environment.etc."web/index.html".text = name;
  };
in
pkgs.testers.runNixOSTest {
  name = "microvm-host";
  meta.timeout = 3600;

  nodes = {
    host = {
      imports = [ module ];
      virtualisation = {
        cores = 4;
        memorySize = 3072;
        diskSize = 4096;
        # The host CPU passes VMX or SVM through, so the guests run under KVM, not in software.
        qemu.options = [
          "-cpu"
          "host"
        ];
      };
      networking.nftables.enable = true;
      networking.firewall.enable = true;
      environment.systemPackages = [ pkgs.openssh ];

      # The secret reaches the host at run time, as a real secret does.
      systemd.tmpfiles.rules = [
        "d /run/test 0700 root root -"
        "f /run/test/token 0400 root root - a-secret-value"
      ];

      tabnix.microvmHost = {
        enable = true;
        gateway = "10.20.0.1";
        uplink = "eth1";
        nameservers = [ ];
        adminKeys = [ keys.snakeOilEd25519PublicKey ];
        guests = {
          a = {
            address = "10.20.0.10";
            mac = "02:00:00:00:00:10";
            memory = 384;
            egress = true;
            config = web "a" [ 7070 ];
          };
          b = {
            address = "10.20.0.20";
            mac = "02:00:00:00:00:20";
            memory = 384;
            secrets.token = "/run/test/token";
            publish = [ 8080 ];
            config = web "b" [
              8080
              9090
            ];
          };
        };
        flows = [
          {
            from = "a";
            to = "b";
            ports = [ 8080 ];
          }
        ];
      };
    };

    # A machine beyond the uplink of the host.
    outside = {
      environment.systemPackages = [ pkgs.curl ];
      # A route to the guests through the host, so the test proves that the host drops, not that no route exists.
      networking.interfaces.eth1.ipv4.routes = [
        {
          address = "10.20.0.0";
          prefixLength = 24;
          via = "192.168.1.1";
        }
      ];
      networking.firewall.allowedTCPPorts = [ 80 ];
      services.nginx = {
        enable = true;
        virtualHosts.default.locations."/".return = "200 outside";
      };
    };
  };

  testScript = ''
    # -n keeps ssh off the stdin of the test driver.
    ssh = "ssh -n -i /root/key -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5"
    guests = {"a": "10.20.0.10", "b": "10.20.0.20"}

    def on(guest, command):
        return f"{ssh} root@{guests[guest]} {command!r}"

    def curl(target, port):
        return f"curl -sf --connect-timeout 5 --max-time 10 http://{target}:{port}/"

    start_all()
    outside.wait_for_unit("nginx.service")
    host.succeed("install -m 0600 ${keys.snakeOilEd25519PrivateKey} /root/key")
    for name in guests:
        host.wait_for_unit(f"microvm@{name}.service", timeout=1800)
    # Nested guests boot slowly on a CI runner that is itself a VM.
    for name, ip in guests.items():
        host.wait_until_succeeds(on(name, "true"), timeout=1800)
    host.wait_until_succeeds(on("a", "systemctl is-active web-7070"), timeout=600)
    host.wait_until_succeeds(on("b", "systemctl is-active web-8080 web-9090"), timeout=600)

    with subtest("A declared flow crosses the host"):
        out = host.succeed(on("a", curl(guests["b"], 8080)))
        assert out.strip() == "b", out

    with subtest("A port that no flow names is closed"):
        host.fail(on("a", curl(guests["b"], 9090)))

    with subtest("A flow opens one direction only"):
        host.fail(on("b", curl(guests["a"], 7070)))
        host.fail(on("b", curl(guests["a"], 22)))

    outside_ip = outside.succeed("ip -4 -o addr show eth1 | awk '{print $4}' | cut -d/ -f1").strip()

    with subtest("A guest with egress reaches the network beyond the host"):
        out = host.succeed(on("a", curl(outside_ip, 80)))
        assert out.strip() == "outside", out

    with subtest("A guest without egress never reaches the network beyond the host"):
        host.fail(on("b", curl(outside_ip, 80)))

    host_ip = host.succeed("ip -4 -o addr show eth1 | awk '{print $4}' | cut -d/ -f1").strip()

    with subtest("A published port reaches its guest from beyond the host"):
        out = outside.succeed(curl(host_ip, 8080))
        assert out.strip() == "b", out

    with subtest("A guest port that is not published stays closed from beyond the host"):
        outside.fail(curl("10.20.0.20", 9090))
        outside.fail(curl("10.20.0.10", 7070))
        outside.fail(curl(host_ip, 9090))

    with subtest("A guest reads its secret and cannot change it"):
        out = host.succeed(on("b", "cat /run/host-secrets/token"))
        assert out.strip() == "a-secret-value", out
        host.fail(on("b", "sh -c 'echo x > /run/host-secrets/token'"))

    with subtest("A guest without secrets sees none of another guest"):
        host.fail(on("a", "cat /run/host-secrets/token"))
  '';
}
