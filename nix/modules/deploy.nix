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

  cfg = config.tabkit.deploy;

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
        check = mkOption {
          type = types.bool;
          default = true;
          description = "Whether the checks of deploy-rs prove this node. A copy for a test, such as a QEMU copy of a real node, can turn it off, because the real node already proves the same modules.";
        };
        confirmTimeout = mkOption {
          type = types.ints.positive;
          default = 300;
          description = "The seconds that the node waits for a new SSH connection before it rolls back.";
        };
        hostKey = mkOption {
          type = types.nullOr (
            types.submodule {
              options = {
                sopsFile = mkOption {
                  type = types.str;
                  example = "infra/secrets/dev/pki.sops.yaml";
                  description = "The encrypted file that holds the SSH host key, relative to the repo root.";
                };
                key = mkOption {
                  type = types.str;
                  example = "SSH_HOST_KEY";
                  description = "The top-level key of the SSH host key in that file.";
                };
              };
            }
          );
          default = null;
          description = "The SSH host key that the install places, so the machine can decrypt its SOPS secrets on first boot.";
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
  options.tabkit.deploy.nodes = mkOption {
    type = types.attrsOf (types.submodule nodeOptions);
    default = { };
    description = "The machines that deploy-rs deploys, by name.";
  };

  config = lib.mkIf (cfg.nodes != { }) {
    flake.deploy.nodes = lib.mapAttrs node cfg.nodes;

    # deploy-rs checks the schema of the nodes and that each profile activates. A NixOS profile builds on Linux only.
    perSystem =
      { pkgs, system, ... }:
      {
        checks = lib.optionalAttrs (lib.hasSuffix "-linux" system) (
          localInputs.deploy-rs.lib.${system}.deployChecks {
            nodes = lib.filterAttrs (name: _: cfg.nodes.${name}.check) config.flake.deploy.nodes;
          }
        );

        # `nix run .#install-<node> -- <target>` erases the disks of the target and installs the node.
        packages = lib.mapAttrs' (
          name: n:
          lib.nameValuePair "install-${name}" (
            pkgs.writeShellApplication {
              name = "install-${name}";
              runtimeInputs = [
                pkgs.nixos-anywhere
                pkgs.openssh
                pkgs.sops
              ];
              text = ''
                target=''${1:?Name the target, such as root@203.0.113.10}
                extra=$(mktemp -d)
                trap 'rm -rf "$extra"' EXIT
                install -d -m 0755 "$extra/etc/ssh"
                sops decrypt --extract ${lib.escapeShellArg ''["${n.hostKey.key}"]''} ${lib.escapeShellArg n.hostKey.sopsFile} \
                  > "$extra/etc/ssh/ssh_host_ed25519_key"
                chmod 0600 "$extra/etc/ssh/ssh_host_ed25519_key"
                ssh-keygen -y -f "$extra/etc/ssh/ssh_host_ed25519_key" > "$extra/etc/ssh/ssh_host_ed25519_key.pub"
                nixos-anywhere --flake ${lib.escapeShellArg ".#${n.configuration}"} --extra-files "$extra" --target-host "$target"
              '';
            }
          )
        ) (lib.filterAttrs (_: n: n.hostKey != null) cfg.nodes);
      };
  };
}
