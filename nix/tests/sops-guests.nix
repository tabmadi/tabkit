# The check of the sops-guests module, per ADR-0500. It evaluates the module with small stand-ins for sops-nix and the
# microVM host, and proves that a change in one encrypted file changes the revision of its guest only.
{ pkgs, module }:
let
  inherit (pkgs) lib;

  stubs =
    { lib, ... }:
    {
      options.sops.templates = lib.mkOption {
        type = lib.types.attrsOf (
          lib.types.submodule (
            { name, ... }:
            {
              options = {
                content = lib.mkOption { type = lib.types.str; };
                path = lib.mkOption {
                  type = lib.types.str;
                  default = "/run/secrets/rendered/${name}";
                };
              };
            }
          )
        );
        default = { };
      };
      options.tabkit.microvmHost.guests = lib.mkOption {
        type = lib.types.attrsOf (
          lib.types.submodule {
            options = {
              secrets = lib.mkOption { type = lib.types.attrsOf lib.types.str; };
              secretsRevision = lib.mkOption { type = lib.types.str; };
            };
          }
        );
        default = { };
      };
    };

  evaluate =
    appFile:
    (lib.evalModules {
      modules = [
        stubs
        module
        {
          tabkit.sopsGuests = {
            enable = true;
            guests = {
              app = {
                sopsFiles = [ appFile ];
                files."app.env" = "TOKEN=placeholder\n";
              };
              db = {
                sopsFiles = [ (builtins.toFile "db.sops.yaml" "db: encrypted\n") ];
                files."roles.sql" = "SELECT 1;\n";
              };
            };
          };
        }
      ];
    }).config;

  first = evaluate (builtins.toFile "app-1.sops.yaml" "app: encrypted-1\n");
  second = evaluate (builtins.toFile "app-2.sops.yaml" "app: encrypted-2\n");
  guests = first.tabkit.microvmHost.guests;
in
assert guests.app.secrets."app.env" == "/run/secrets/rendered/app/app.env";
assert guests.db.secrets."roles.sql" == "/run/secrets/rendered/db/roles.sql";
assert first.sops.templates."app/app.env".content == "TOKEN=placeholder\n";
assert guests.app.secretsRevision != second.tabkit.microvmHost.guests.app.secretsRevision;
assert guests.db.secretsRevision == second.tabkit.microvmHost.guests.db.secretsRevision;
pkgs.runCommand "sops-guests" { } "touch $out"
