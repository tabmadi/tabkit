# The full tier of a self-hosted repo, per ADR-0500: the real host in QEMU with two mirrored disks, its installer, its
# install, its deploy, and its stop. Each tier is one package, `full-tier-<name>`.
localInputs:
{
  config,
  lib,
  ...
}:
let
  inherit (lib) mkOption types;

  cfg = config.tabkit.fullTier;

  tierOptions =
    { name, ... }:
    {
      options = {
        configuration = mkOption {
          type = types.str;
          default = name;
          description = "The nixosConfiguration of this flake that the tier installs.";
        };
        node = mkOption {
          type = types.str;
          default = name;
          description = "The deploy node of this flake that the tier deploys.";
        };
        cpus = mkOption {
          type = types.ints.positive;
          default = 8;
          description = "The virtual CPUs of the tier.";
        };
        memory = mkOption {
          type = types.ints.positive;
          default = 12288;
          description = "The memory of the tier, in MiB.";
        };
        diskSize = mkOption {
          type = types.ints.positive;
          default = 40;
          description = "The size of each disk, in GiB.";
        };
        disks = mkOption {
          type = types.enum [
            1
            2
          ];
          default = 2;
          description = "The number of disks: two for a mirrored host, one for a host with a single disk.";
        };
        minFreeMemory = mkOption {
          type = types.ints.positive;
          default = 14;
          description = "The free memory, in GiB, that a start needs.";
        };
        minFreeDisk = mkOption {
          type = types.ints.positive;
          default = 100;
          description = "The free disk, in GiB, that a start needs.";
        };
        sshPort = mkOption {
          type = types.port;
          default = 2222;
          description = "The local port that reaches SSH of the tier.";
        };
        forwards = mkOption {
          type = types.attrsOf types.port;
          default = { };
          example = {
            "8443" = 443;
          };
          description = "Other local TCP ports, by name, and the port of the tier that each one reaches.";
        };
        mac = mkOption {
          type = types.str;
          default = "52:54:00:12:34:56";
          description = "The MAC address of the uplink of the tier.";
        };
        sshKey = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "~/.ssh/full-tier";
          description = "The private SSH key that reaches the tier, as a path at run time.";
        };
        hostKey = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "infra/full-tier/ssh_host_ed25519_key";
          description = "The SSH host key of the tier, as a path at run time. It is never the key of a real host.";
        };
        adminKeys = mkOption {
          type = types.listOf types.str;
          default = [ ];
          description = "The SSH public keys that the installer accepts.";
        };
      };
    };

  # The installer is its own package, so a build of the commands never builds an ISO.
  installer =
    system: tier:
    let
      path = config.flake.nixosConfigurations.${tier.configuration}.pkgs.path;
    in
    (import "${path}/nixos/lib/eval-config.nix" {
      inherit system;
      modules = [
        "${path}/nixos/modules/installer/cd-dvd/installation-cd-minimal.nix"
        {
          users.users.root.openssh.authorizedKeys.keys = tier.adminKeys;
          services.openssh.settings.PasswordAuthentication = false;
          isoImage.squashfsCompression = "zstd -Xcompression-level 3";
        }
      ];
    }).config.system.build.isoImage;

  package =
    pkgs: system: name: tier:
    let
      forwards = lib.concatStringsSep "," (
        [ "hostfwd=tcp:127.0.0.1:${toString tier.sshPort}-:22" ]
        ++ lib.mapAttrsToList (
          local: remote: "hostfwd=tcp:127.0.0.1:${local}-:${toString remote}"
        ) tier.forwards
      );
    in
    pkgs.writeShellApplication {
      name = "full-tier-${name}";
      runtimeInputs = [
        localInputs.deploy-rs.packages.${system}.default
        pkgs.coreutils
        pkgs.findutils
        pkgs.gawk
        pkgs.gnugrep
        pkgs.nixos-anywhere
        pkgs.openssh
        pkgs.qemu_kvm
        pkgs.socat
      ];
      text = ''
        export FULL_TIER_NAME=${lib.escapeShellArg name}
        export FULL_TIER_CONFIGURATION=${lib.escapeShellArg tier.configuration}
        export FULL_TIER_NODE=${lib.escapeShellArg tier.node}
        export FULL_TIER_CPUS=${toString tier.cpus}
        export FULL_TIER_MEMORY_MIB=${toString tier.memory}
        export FULL_TIER_DISK_GIB=${toString tier.diskSize}
        export FULL_TIER_DISKS=${if tier.disks == 2 then "'a b'" else "a"}
        export FULL_TIER_MIN_MEMORY_GIB=''${FULL_TIER_MIN_MEMORY_GIB:-${toString tier.minFreeMemory}}
        export FULL_TIER_MIN_DISK_GIB=''${FULL_TIER_MIN_DISK_GIB:-${toString tier.minFreeDisk}}
        export FULL_TIER_SSH_PORT=${toString tier.sshPort}
        export FULL_TIER_FORWARDS=${lib.escapeShellArg forwards}
        export FULL_TIER_MAC=${lib.escapeShellArg tier.mac}
        export FULL_TIER_SSH_KEY=''${FULL_TIER_SSH_KEY:-${
          if tier.sshKey == null then "" else lib.replaceStrings [ "~" ] [ "$HOME" ] tier.sshKey
        }}
        export FULL_TIER_HOST_KEY=''${FULL_TIER_HOST_KEY:-${
          if tier.hostKey == null then "" else tier.hostKey
        }}
        ${builtins.readFile ./full-tier.sh}
      '';
    };
in
{
  options.tabkit.fullTier = mkOption {
    type = types.attrsOf (types.submodule tierOptions);
    default = { };
    description = "The full tiers of this repo, by name.";
  };

  config.perSystem =
    { pkgs, system, ... }:
    lib.mkIf (system == "x86_64-linux" && cfg != { }) {
      packages = lib.concatMapAttrs (name: tier: {
        "full-tier-${name}" = package pkgs system name tier;
        "full-tier-${name}-installer" = installer system tier;
      }) cfg;
    };
}
