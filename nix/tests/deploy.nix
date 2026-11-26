# The check of the deploy module of ADR-0500. A fixture flake deploys one small machine, and the check asserts the
# rollback settings, keeps a node with `check = false` out of the checks, and runs the checks of deploy-rs.
{
  pkgs,
  inputs,
  deploy,
}:
let
  inherit (pkgs.stdenv.hostPlatform) system;

  machine = inputs.nixpkgs.lib.nixosSystem {
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

  fixture = inputs.flake-parts.lib.mkFlake { inherit inputs; } {
    systems = [ system ];
    imports = [ deploy ];
    flake.nixosConfigurations.machine = machine;
    # A copy for a test lives outside nixosConfigurations, so nix flake check skips it.
    flake.labConfigurations.machine = machine;
    tabkit.deploy.nodes = {
      machine.hostname = "192.0.2.10";
      # A copy for a test deploys, but stays out of the checks.
      copy = {
        hostname = "127.0.0.1";
        machine = "labConfigurations.machine";
        check = false;
      };
    };
  };

  node = fixture.deploy.nodes.machine;
  activateCommand =
    builtins.unsafeDiscardStringContext
      fixture.checks.${system}.deploy-activate.buildCommand;
in
assert node.magicRollback && node.autoRollback;
assert node.sshUser == "root" && node.confirmTimeout == 300;
assert fixture.deploy.nodes ? copy;
# The copy runs the machine at its own attribute path.
assert fixture.deploy.nodes.copy.profiles.system.path == node.profiles.system.path;
# deploy-activate lists each node that it proves as path:node:profile.
assert pkgs.lib.hasInfix ":machine:system" activateCommand;
assert !(pkgs.lib.hasInfix ":copy:system" activateCommand);
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
