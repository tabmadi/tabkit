# The shared toolchain, per ADR-0200: the base dev shell, the git hooks, and the checks of every repo.
# The tabkit flake applies its own inputs, so a repo never adds git-hooks.nix or tabkit itself.
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
      cfg = config.tabkit;

      # The configs in .config/ that a tool finds only through an environment variable or a flag.
      configEnv.GITLEAKS_CONFIG = ".config/gitleaks.toml";
      cog = "${pkgs.cocogitto}/bin/cog --config .config/cog.toml";
      git = "${pkgs.gitMinimal}/bin/git";

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

      # A rebase or a cherry-pick can bring a bad message past the commit-msg hook, so CI checks the range of a branch.
      commitRange = pkgs.writeShellScriptBin "commit-range" ''
        set -euo pipefail
        base=''${BASE_REF:-master}
        base=''${base#refs/heads/}
        ref=""
        for candidate in "origin/$base" "$base"; do
          if ${git} rev-parse --verify --quiet "$candidate^{commit}" > /dev/null; then
            ref=$candidate
            break
          fi
        done
        if [ -z "$ref" ]; then
          echo "⚠ no such base ref: $base. Skipping the commit-range check" >&2
          exit 0
        fi
        range="$ref..HEAD"
        if [ "$(${git} rev-list --count "$range")" -eq 0 ]; then
          echo "✓ no commits ahead of $ref"
          exit 0
        fi
        ${cog} check --ignore-merge-commits "$range"
        echo "✓ commit messages conform to Conventional Commits"
      '';

      hooks = localInputs.git-hooks.lib.${system}.run {
        src = self;
        package = pkgs.prek;
        hooks = {
          markdown = {
            enable = true;
            name = "rumdl";
            entry = "${pkgs.rumdl}/bin/rumdl check";
            files = "\\.md$";
          };
          data = {
            enable = true;
            name = "dprint";
            entry = "${dprint}/bin/dprint check --allow-no-files";
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
            name = "cog verify, title only";
            # `git commit --verbose` puts an uncommented diff after the scissors line.
            entry = "${pkgs.writeShellScript "commit-title-only" ''
              ${cog} verify --file "$1" || exit 1
              body=$(${pkgs.gnused}/bin/sed '/^. -* >8 -*$/,$d' "$1" | ${git} stripspace --strip-comments | ${pkgs.coreutils}/bin/tail -n +2)
              if [ -n "$body" ]; then
                echo "A commit message is a title only, with no body." >&2
                exit 1
              fi
            ''}";
            stages = [ "commit-msg" ];
          };
          # Only the commits that this push adds. Older history can predate the convention.
          # Before the first push, origin/master does not exist, so every commit is checked.
          commit-range = {
            enable = true;
            name = "cog check, title only";
            entry = "${pkgs.writeShellScript "cog-check-range" ''
              if ${git} rev-parse --quiet --verify origin/master > /dev/null; then
                range=origin/master..HEAD
                ${cog} check "$range" || exit 1
              else
                range=HEAD
                ${cog} check || exit 1
              fi
              status=0
              for commit in $(${git} rev-list "$range"); do
                if [ -n "$(${git} log -1 --format=%b "$commit")" ]; then
                  echo "Commit $commit has a body. A commit message is a title only." >&2
                  status=1
                fi
              done
              exit "$status"
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
        // lib.optionalAttrs cfg.rules.enable {
          prose = {
            enable = true;
            name = "tabkit";
            entry = "${lib.getExe cfg.rules.package} check";
            pass_filenames = false;
          };
        }
        // lib.mapAttrs (_: hook: { enable = true; } // hook) cfg.hooks;
      };
    in
    {
      options.tabkit = {
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
        rules = {
          enable = mkOption {
            type = types.bool;
            default = true;
            description = "Whether the dev shell, the hooks, and the checks hold the tabkit binary.";
          };
          package = mkOption {
            type = types.package;
            default = localInputs.self.packages.${system}.default;
            defaultText = lib.literalMD "tabkit, from this flake";
            description = "The tabkit binary. A repo can set another build.";
          };
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
            pkgs.nixd
            pkgs.nixfmt
            (import ./nix-cache-push.nix { inherit pkgs; })
            commitRange
            (import ./labels.nix { inherit pkgs; })
          ]
          ++ lib.optional cfg.rules.enable cfg.rules.package
          ++ cfg.packages;
          env = configEnv // cfg.env;
          shellHook = hooks.shellHook;
        };

        checks = {
          lint-md = check "lint-md" [ pkgs.rumdl ] "rumdl check .";
          lint-secrets = check "lint-secrets" [ pkgs.gitleaks ] "gitleaks dir . --no-banner";
          lint-data = check "lint-data" [ dprint ] "DPRINT_CACHE_DIR=$TMPDIR/dprint dprint check";
          lint-nix = check "lint-nix" [ pkgs.nixfmt ] "find . -name '*.nix' -exec nixfmt --check {} +";
          lint-just = check "lint-just" [ pkgs.just ] "just --fmt --check";
          # Event text in a `run` script is an injection. A repo lists its extra runner labels in the config.
          lint-workflows = check "lint-workflows" [ pkgs.actionlint ] ''
            if [ -d .github/workflows ]; then
              config=()
              if [ -f .config/actionlint.yaml ]; then
                config=(-config-file .config/actionlint.yaml)
              fi
              actionlint -shellcheck= -pyflakes= "''${config[@]}" .github/workflows/*.yml
            fi
          '';
        }
        // lib.optionalAttrs cfg.rules.enable {
          # tabkit reads the recipes through `just`.
          lint-prose = check "lint-prose" [
            cfg.rules.package
            pkgs.just
          ] "tabkit check";
        };

        formatter = pkgs.nixfmt;
      };
    };
}
