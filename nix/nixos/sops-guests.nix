# SOPS templates mapped to the secret shares of microVM guests, per ADR-0500. A repo imports sops-nix and names the
# content of each file. A change in an encrypted file restarts only the guests that read from it.
{
  config,
  lib,
  ...
}:
let
  inherit (lib) mkOption types;

  cfg = config.tabkit.sopsGuests;

  revision =
    guest:
    builtins.hashString "sha256" (
      lib.concatMapStrings (builtins.hashFile "sha256") cfg.guests.${guest}.sopsFiles
    );

  fileOptions = {
    options = {
      sopsFiles = mkOption {
        type = types.listOf types.path;
        default = [ ];
        description = "The encrypted files that this guest's secrets come from. A change in one restarts the guest.";
      };
      # Lazy, because a placeholder of sops-nix depends on the list of templates that these names create.
      files = mkOption {
        type = types.lazyAttrsOf types.str;
        default = { };
        example = {
          "app.env" = "TOKEN=<a sops placeholder>\n";
        };
        description = "The content of each secret file of the guest, by name, with sops placeholders.";
      };
    };
  };
in
{
  options.tabkit.sopsGuests = {
    enable = mkOption {
      type = types.bool;
      default = false;
      description = "Whether SOPS templates fill the secret shares of the guests.";
    };
    guests = mkOption {
      type = types.attrsOf (types.submodule fileOptions);
      default = { };
      description = "The secret files of each guest, by the name of the guest.";
    };
  };

  config = lib.mkIf cfg.enable {
    sops.templates = lib.mkMerge (
      lib.mapAttrsToList (
        guest: value:
        lib.mapAttrs' (file: content: lib.nameValuePair "${guest}/${file}" { inherit content; }) value.files
      ) cfg.guests
    );

    tabkit.microvmHost.guests = lib.mapAttrs (guest: value: {
      secrets = lib.mapAttrs (file: _: config.sops.templates."${guest}/${file}".path) value.files;
      secretsRevision = revision guest;
    }) cfg.guests;
  };
}
