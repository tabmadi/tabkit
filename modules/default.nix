# The shared toolchain, per ADR-0200: the base dev shell, the git hooks, and the checks of every repo.
# The tabnix flake applies its own inputs, so a repo never adds git-hooks.nix or tablint itself.
localInputs:
{ self, lib, ... }:
let
  inherit (lib) mkOption types;
in
{
  systems = lib.mkDefault [
    "x86_64-linux"
    "aarch64-linux"
  ];

  perSystem =
    {
      config,
      pkgs,
      system,
      ...
    }:
    let
      cfg = config.tabnix;

      # The configs in .config/ that a tool finds only through an environment variable or a flag.
      configEnv.GITLEAKS_CONFIG = ".config/gitleaks.toml";
      cog = "${pkgs.cocogitto}/bin/cog --config .config/cog.toml";

      # dprint takes its config and plugins only as flags after the subcommand, so the wrapper adds both.
      dprint =
        let
          plugins = with pkgs.dprint-plugins; [
            dprint-plugin-json
            dprint-plugin-toml
            g-plane-pretty_yaml
          ];
          pluginFiles = lib.concatMapStringsSep " " (plugin: "${plugin}/plugin.wasm") plugins;
        in
        pkgs.writeShellScriptBin "dprint" ''
          case "''${1:-}" in
            fmt | check | file-paths | output-file-paths | resolved-config | output-resolved-config)
              command=$1
              shift
              exec ${pkgs.dprint}/bin/dprint "$command" --plugins ${pluginFiles} --config .config/dprint.json "$@"
              ;;
            *) exec ${pkgs.dprint}/bin/dprint "$@" ;;
          esac
        '';

      # A check runs a command on a writable copy of the tracked files, so a tool can write its cache.
      check =
        name: tools: command:
        pkgs.runCommand name ({ nativeBuildInputs = tools; } // configEnv) ''
          cp -r ${self} source
          chmod -R u+w source
          cd source
          ${command}
          touch $out
        '';

      hooks = localInputs.git-hooks.lib.${system}.run {
        src = self;
        package = pkgs.prek;
        hooks = {
          prose = {
            enable = true;
            name = "tablint";
            entry = "${lib.getExe cfg.tablint} check";
            pass_filenames = false;
          };
          markdown = {
            enable = true;
            name = "rumdl";
            entry = "${pkgs.rumdl}/bin/rumdl check";
            files = "\\.md$";
          };
          data = {
            enable = true;
            name = "dprint";
            entry = "${dprint}/bin/dprint check";
            files = "\\.(jsonc?|toml|ya?ml)$";
          };
          nix = {
            enable = true;
            name = "nixfmt";
            entry = "${pkgs.nixfmt}/bin/nixfmt --check";
            files = "\\.nix$";
          };
          justfile = {
            enable = true;
            name = "just --fmt";
            entry = "${pkgs.just}/bin/just --fmt --check";
            files = "(^|/)justfile$";
            pass_filenames = false;
          };
          secrets = {
            enable = true;
            name = "gitleaks";
            entry = "${pkgs.gitleaks}/bin/gitleaks git --config ${configEnv.GITLEAKS_CONFIG} --pre-commit --staged --no-banner";
            pass_filenames = false;
          };
          commit-message = {
            enable = true;
            name = "cog verify";
            entry = "${cog} verify --file";
            stages = [ "commit-msg" ];
          };
          # Only the commits that this push adds. Older history can predate the convention.
          # Before the first push, origin/master does not exist, so every commit is checked.
          commit-range = {
            enable = true;
            name = "cog check";
            entry = "${pkgs.writeShellScript "cog-check-range" ''
              if ${pkgs.gitMinimal}/bin/git rev-parse --quiet --verify origin/master > /dev/null; then
                exec ${cog} check origin/master..HEAD
              fi
              exec ${cog} check
            ''}";
            stages = [ "pre-push" ];
            pass_filenames = false;
            always_run = true;
          };
          ci = {
            enable = true;
            name = "just ci";
            entry = "${pkgs.just}/bin/just ci";
            stages = [ "pre-push" ];
            pass_filenames = false;
            always_run = true;
          };
        }
        // lib.mapAttrs (_: hook: { enable = true; } // hook) cfg.hooks;
      };
    in
    {
      options.tabnix = {
        packages = mkOption {
          type = types.listOf types.package;
          default = [ ];
          description = "The language tools of the repo. They join the base dev shell.";
        };
        hooks = mkOption {
          type = types.attrsOf types.attrs;
          default = { };
          description = "The language hooks of the repo, in the git-hooks.nix format. They join the shared hooks.";
        };
        compiler = mkOption {
          type = types.bool;
          default = false;
          description = "Whether the dev shell holds a C compiler, which a cargo or cgo build needs.";
        };
        env = mkOption {
          type = types.attrsOf types.str;
          default = { };
          description = "The environment variables of the repo. They join the variables of the base dev shell.";
        };
        tablint = mkOption {
          type = types.package;
          default = localInputs.tablint.packages.${system}.default;
          description = "The tablint binary. The tablint repo sets its own build here.";
        };
        check = mkOption {
          type = types.raw;
          readOnly = true;
          default = check;
          description = "The check helper: a name, a list of tools, and a command that runs on a copy of the tracked files.";
        };
      };

      config = {
        devShells.default = (if cfg.compiler then pkgs.mkShell else pkgs.mkShellNoCC) {
          packages = [
            pkgs.just
            pkgs.cocogitto
            pkgs.rumdl
            pkgs.gitleaks
            dprint
            cfg.tablint
            pkgs.nixd
            pkgs.nixfmt
            (import ./nix-cache-push.nix { inherit pkgs; })
          ]
          ++ cfg.packages;
          env = configEnv // cfg.env;
          shellHook = hooks.shellHook;
        };

        checks = {
          lint-prose = check "lint-prose" [ cfg.tablint ] "tablint check";
          lint-md = check "lint-md" [ pkgs.rumdl ] "rumdl check .";
          lint-secrets = check "lint-secrets" [ pkgs.gitleaks ] "gitleaks dir . --no-banner";
          lint-data = check "lint-data" [ dprint ] "DPRINT_CACHE_DIR=$TMPDIR/dprint dprint check";
          lint-nix = check "lint-nix" [ pkgs.nixfmt ] "find . -name '*.nix' -exec nixfmt --check {} +";
          lint-just = check "lint-just" [ pkgs.just ] "just --fmt --check";
        };

        formatter = pkgs.nixfmt;
      };
    };
}
