# The check of the full-tier module, per ADR-0500. A fixture flake builds one tier. The check proves that a start
# refuses to run without free memory, and that a stop never touches a QEMU process of another state folder.
{
  pkgs,
  inputs,
  fullTier,
}:
let
  inherit (pkgs.stdenv.hostPlatform) system;

  fixture = inputs.flake-parts.lib.mkFlake { inherit inputs; } {
    systems = [ system ];
    imports = [ fullTier ];
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
    tabkit.fullTier.machine = {
      adminKeys = [ "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample test" ];
      forwards."8443" = 443;
    };
  };

  tier = fixture.packages.${system}.full-tier-machine;
in
pkgs.runCommand "module-full-tier" { nativeBuildInputs = [ pkgs.coreutils ]; } ''
  export HOME=$TMPDIR
  export FULL_TIER_STATE=$TMPDIR/state
  mkdir -p "$FULL_TIER_STATE"

  # A process of another tier, recorded in the pid file of this one.
  sleep 60 &
  other=$!
  echo "$other" > "$FULL_TIER_STATE/qemu.pid"

  test "$(${tier}/bin/full-tier-machine status)" = stopped
  ${tier}/bin/full-tier-machine stop
  kill -0 "$other"
  kill "$other"
  rm -f "$FULL_TIER_STATE/qemu.pid"

  if FULL_TIER_MIN_MEMORY_GIB=999999 ${tier}/bin/full-tier-machine start 2> error; then
    echo "A start ran without free memory." >&2
    exit 1
  fi
  grep -q 'GiB of free memory' error
  test ! -e "$FULL_TIER_STATE/disk-a.qcow2"

  if ${tier}/bin/full-tier-machine unknown 2> /dev/null; then
    exit 1
  fi
  touch $out
''
