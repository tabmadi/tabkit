# The deploy of NixOS machines, per ADR-0500. Each node deploys one nixosConfiguration of the flake with deploy-rs.
# A deploy that loses SSH rolls back by itself, and a check proves the deploy before a push.
localInputs:
{
  config,
  lib,
  ...
}:
let
  inherit (lib) mkOption types;

  cfg = config.tabnix.deploy;

  nodeOptions =
    { name, ... }:
    {
      options = {
        hostname = mkOption {
          type = types.str;
          example = "203.0.113.10";
          description = "The address or SSH host name that the deploy connects to.";
        };
        configuration = mkOption {
          type = types.str;
          default = name;
          description = "The name of the nixosConfiguration of this flake that the node runs.";
        };
        sshUser = mkOption {
          type = types.str;
          default = "root";
          description = "The user that the deploy logs in as.";
        };
        sshOpts = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [
            "-p"
            "2222"
          ];
          description = "The extra options of each SSH connection, such as a port of a local QEMU copy.";
        };
        activationTimeout = mkOption {
          type = types.ints.positive;
          default = 900;
          description = "The seconds that an activation may take. A restart of each guest of a host fits in it.";
        };
        confirmTimeout = mkOption {
          type = types.ints.positive;
          default = 300;
          description = "The seconds that the node waits for a new SSH connection before it rolls back.";
        };
      };
    };

  node =
    name: n:
    let
      nixos = config.flake.nixosConfigurations.${n.configuration};
      inherit (nixos.pkgs.stdenv.hostPlatform) system;
    in
    {
      inherit (n)
        hostname
        sshUser
        sshOpts
        activationTimeout
        confirmTimeout
        ;
      magicRollback = true;
      autoRollback = true;
      profiles.system = {
        user = "root";
        path = localInputs.deploy-rs.lib.${system}.activate.nixos nixos;
      };
    };
in
{
  options.tabnix.deploy.nodes = mkOption {
    type = types.attrsOf (types.submodule nodeOptions);
    default = { };
    description = "The machines that deploy-rs deploys, by name.";
  };

  config = lib.mkIf (cfg.nodes != { }) {
    flake.deploy.nodes = lib.mapAttrs node cfg.nodes;

    # deploy-rs checks the schema of the nodes and that each profile activates. A NixOS profile builds on Linux only.
    perSystem =
      { system, ... }:
      {
        checks = lib.optionalAttrs (lib.hasSuffix "-linux" system) (
          localInputs.deploy-rs.lib.${system}.deployChecks config.flake.deploy
        );
      };
  };
}
