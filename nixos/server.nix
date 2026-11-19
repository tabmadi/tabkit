# The base of a server, per ADR-0500: SSH with keys only, the nftables firewall, and Nix with flakes and a
# weekly garbage collection. A deploy tool reaches the server as root with a key.
{ config, lib, ... }:
let
  inherit (lib) mkOption types;

  cfg = config.tabnix.server;
in
{
  options.tabnix.server = {
    enable = mkOption {
      type = types.bool;
      default = false;
      description = "Whether the machine takes the server base of this module.";
    };
    adminKeys = mkOption {
      type = types.nonEmptyListOf types.str;
      description = "The SSH public keys that log in as root. No password logs in.";
    };
    keepGenerations = mkOption {
      type = types.str;
      default = "30d";
      description = "How long the garbage collection keeps an old system generation, so a rollback can reach it.";
    };
  };

  config = lib.mkIf cfg.enable {
    time.timeZone = lib.mkDefault "UTC";

    services.openssh = {
      enable = true;
      openFirewall = true;
      settings = {
        PasswordAuthentication = false;
        KbdInteractiveAuthentication = false;
        PermitRootLogin = "prohibit-password";
      };
    };
    users.users.root.openssh.authorizedKeys.keys = cfg.adminKeys;
    users.mutableUsers = false;

    networking.nftables.enable = true;
    networking.firewall.enable = true;

    nix.settings = {
      experimental-features = [
        "nix-command"
        "flakes"
      ];
      trusted-users = [ "root" ];
    };
    nix.gc = {
      automatic = true;
      dates = "weekly";
      options = "--delete-older-than ${cfg.keepGenerations}";
    };
  };
}
