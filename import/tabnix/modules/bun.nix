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

      # The hook of bun2nix brings the Bun of its own nixpkgs. A check runs the Bun of the dev shell, so it takes that one.
      hook = bun2nix.hook.overrideAttrs (old: {
        propagatedBuildInputs = map (
          input: if (input.pname or "") == "bun" then cfg.package else input
        ) old.propagatedBuildInputs;
      });

      # The attributes of a derivation that installs bun.lock from bun.nix and runs no phase of its own.
      install = {
        src = self;
        nativeBuildInputs = [ hook ] ++ cfg.tools;
        bunDeps = bun2nix.fetchBunDeps { bunNix = linuxOnly "${self}/bun.nix"; };
        dontUseBunBuild = true;
        dontUseBunCheck = true;
        dontUseBunInstall = true;
        dontRunLifecycleScripts = true;
        env = cfg.env;
      }
      // lib.optionalAttrs (cfg.installFlags != [ ]) { bunInstallFlags = cfg.installFlags; };

      manifest = "${self}/package.json";
      packageManager =
        if builtins.pathExists manifest then
          (builtins.fromJSON (builtins.readFile manifest)).packageManager or null
        else
          null;

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
        package = mkOption {
          type = types.package;
          # The recipe of nixpkgs at the newest release of Bun, with the hashes of its SHASUMS256.txt. A new release of
          # Bun is a change here, and nixpkgs may lag it by days.
          default = pkgs.bun.overrideAttrs (
            finalAttrs: old: {
              version = "1.4.5";
              # The source comes from the sources below, as in the recipe of nixpkgs, so the version and the source move together.
              src = finalAttrs.passthru.sources.${pkgs.stdenvNoCC.hostPlatform.system};
              passthru = old.passthru // {
                sources = {
                  x86_64-linux = pkgs.fetchurl {
                    url = "https://github.com/oven-sh/bun/releases/download/bun-v1.4.5/bun-linux-x64-baseline.zip";
                    hash = "sha256-3LI6GKHC//EtV7zvaW27tC62f94p9DgW7LWVRZokxGM=";
                  };
                  aarch64-linux = pkgs.fetchurl {
                    url = "https://github.com/oven-sh/bun/releases/download/bun-v1.4.5/bun-linux-aarch64.zip";
                    hash = "sha256-GGVqO/DqVkH1eWrkITt1JVV5pwoP3tLzN5M3hVaM34Q=";
                  };
                };
              };
            }
          );
          defaultText = lib.literalMD "Bun 1.4.5 from its release";
          description = "The Bun of the dev shell and of every Bun check. A repo takes Bun from here, never from pkgs.bun.";
        };
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
          cfg.package
          bun2nix
        ];

        # A repo that names its Bun in package.json names the Bun of the dev shell.
        checks = lib.optionalAttrs (packageManager != null) {
          bun-version =
            pkgs.runCommand "bun-version"
              {
                inherit packageManager;
                expected = "bun@${cfg.package.version}";
              }
              ''
                if [ "$packageManager" != "$expected" ]; then
                  echo "packageManager is $packageManager, and the dev shell has $expected" >&2
                  exit 1
                fi
                touch $out
              '';
        };

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
