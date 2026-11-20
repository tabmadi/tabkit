# The Bun parts, per ADR-0400: Bun and bun2nix in the dev shell, the hook that keeps bun.nix in step with bun.lock,
# and the install and check helpers that give a build the locked node_modules without network.
localInputs:
{ self, lib, ... }:
let
  inherit (lib) mkOption types;
in
{
  perSystem =
    {
      config,
      pkgs,
      system,
      ...
    }:
    let
      cfg = config.tabnix.bun;
      bun2nix = localInputs.bun2nix.packages.${system}.default;

      # bun.nix lists the native packages of every platform, and a fetch downloads each one. Linux is the one
      # supported platform, per ADR-0100, and Bun skips an optional package of another platform on install.
      foreignPlatform =
        name:
        builtins.match ".*(^|[-/@])(darwin|win32|freebsd|openbsd|netbsd|sunos|android|aix)([-@].*|$)" name
        != null;
      linuxOnly =
        bunNix:
        let
          original = import bunNix;
        in
        lib.setFunctionArgs (args: lib.filterAttrs (name: _: !foreignPlatform name) (original args)) (
          lib.functionArgs original
        );

      # The attributes of a derivation that installs bun.lock from bun.nix and runs no phase of its own.
      install = {
        src = self;
        nativeBuildInputs = [ bun2nix.hook ] ++ cfg.tools;
        bunDeps = bun2nix.fetchBunDeps { bunNix = linuxOnly "${self}/bun.nix"; };
        dontUseBunBuild = true;
        dontUseBunCheck = true;
        dontUseBunInstall = true;
        dontRunLifecycleScripts = true;
        env = cfg.env;
      }
      // lib.optionalAttrs (cfg.installFlags != [ ]) { bunInstallFlags = cfg.installFlags; };

      # A check runs a command on the tracked files with the locked node_modules.
      check =
        name: tools: command:
        pkgs.stdenv.mkDerivation (
          install
          // {
            inherit name;
            nativeBuildInputs = install.nativeBuildInputs ++ tools;
            buildPhase = ''
              export HOME=$TMPDIR
              ${command}
            '';
            installPhase = "touch $out";
          }
        );
    in
    {
      options.tabnix.bun = {
        tools = mkOption {
          type = types.listOf types.package;
          default = [ ];
          description = "The tools that every Bun derivation holds, such as the Node.js that runs the app.";
        };
        installFlags = mkOption {
          type = types.listOf types.str;
          default = [ ];
          description = "The flags of the install, such as `--linker=hoisted`. Empty means the default of bun2nix.";
        };
        env = mkOption {
          type = types.attrsOf types.str;
          default = { };
          description = "The environment variables of every Bun derivation.";
        };
        install = mkOption {
          type = types.raw;
          readOnly = true;
          default = install;
          description = "Read only. The attributes of a derivation that installs the locked node_modules, to extend with its own phases.";
        };
        check = mkOption {
          type = types.raw;
          readOnly = true;
          default = check;
          description = "Read only. Makes a check from a name, a list of tools, and a command that runs with the locked node_modules.";
        };
      };

      config = {
        tabnix.packages = [
          pkgs.bun
          bun2nix
        ];

        # bun.nix follows bun.lock, so a dependency change cannot miss it.
        tabnix.hooks.bun-nix = {
          name = "bun2nix";
          entry = "${pkgs.writeShellScript "bun-nix-check" ''
            ${bun2nix}/bin/bun2nix -o bun.nix
            ${pkgs.nixfmt}/bin/nixfmt bun.nix
            ${pkgs.gitMinimal}/bin/git diff --exit-code -- bun.nix
          ''}";
          files = "(^|/)(bun\\.lock|bun\\.nix)$";
          pass_filenames = false;
        };
      };
    };
}
