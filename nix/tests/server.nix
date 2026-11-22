# The nixosTest of the server base, per ADR-0500. It proves that only a listed key logs in, and that the firewall
# refuses a port that no rule opens.
{ pkgs, module }:
let
  keys = import "${pkgs.path}/nixos/tests/ssh-keys.nix" pkgs;
in
pkgs.testers.runNixOSTest {
  name = "server";

  nodes = {
    server = {
      imports = [ module ];
      tabkit.server = {
        enable = true;
        adminKeys = [ keys.snakeOilEd25519PublicKey ];
      };
      # A user with a password exists, so the test proves that SSH refuses the password, not that it is empty.
      users.users.alice = {
        isNormalUser = true;
        password = "a-password";
      };
      systemd.services.closed = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 -m http.server 8080";
      };
    };
    client = {
      environment.systemPackages = [
        pkgs.sshpass
        pkgs.curl
      ];
    };
  };

  testScript = ''
    # The -n flag keeps ssh off stdin, which carries the commands of the test driver.
    ssh = "ssh -n -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5"

    start_all()
    server.wait_for_unit("sshd.service")
    server.wait_for_unit("closed.service")
    server.wait_for_open_port(8080)
    client.wait_for_unit("multi-user.target")
    client.succeed("install -m 0600 ${keys.snakeOilEd25519PrivateKey} /root/key")

    with subtest("A listed key logs in as root"):
        client.succeed(f"{ssh} -i /root/key root@server true")

    with subtest("A password never logs in"):
        client.fail(f"sshpass -p a-password {ssh} -o PubkeyAuthentication=no alice@server true")

    with subtest("The firewall refuses a port that no rule opens"):
        client.fail("curl -s --connect-timeout 5 http://server:8080/")

    with subtest("Nix on the server takes flakes"):
        server.succeed("nix config show experimental-features | grep flakes")
  '';
}
