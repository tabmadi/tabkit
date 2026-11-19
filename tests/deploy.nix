# The check of the deploy module of ADR-0500. A fixture flake deploys one small machine, and the check asserts the
# rollback settings and runs the checks of deploy-rs.
{
  pkgs,
  inputs,
  deploy,
}:
let
  inherit (pkgs.stdenv.hostPlatform) system;

  fixture = inputs.flake-parts.lib.mkFlake { inherit inputs; } {
    systems = [ system ];
    imports = [ deploy ];
    flake.nixosConfigurations.machine = inputs.nixpkgs.lib.nixosSystem {
      inherit system;
      modules = [
        {
          system.stateVersion = "26.05";
          boot.loader.grub.enable = false;
          fileSystems."/" = {
            device = "/dev/vda";
            fsType = "ext4";
          };
          documentation.enable = false;
        }
      ];
    };
    tabnix.deploy.nodes.machine.hostname = "192.0.2.10";
  };

  node = fixture.deploy.nodes.machine;
in
assert node.magicRollback && node.autoRollback;
assert node.sshUser == "root" && node.confirmTimeout == 300;
pkgs.runCommand "module-deploy"
  {
    schema = fixture.checks.${system}.deploy-schema;
    activate = fixture.checks.${system}.deploy-activate;
  }
  ''
    test -e $schema
    test -e $activate
    touch $out
  ''
