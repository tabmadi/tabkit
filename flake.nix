# The toolchain, the git hooks, the tabrules package, and the checks, per ADR-0100. The tabnix module
# holds every shared part, and this file adds the Rust parts.
{
  description = "Opinionated checks and generators for the files of a repository";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    # tabnix uses tabrules, so its tabrules input follows this flake and never pins an older release.
    tabnix = {
      url = "github:tabmadi/tabnix/v0.11.0";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-parts.follows = "flake-parts";
      inputs.tabrules.follows = "";
    };
  };

  outputs =
    inputs@{ flake-parts, tabnix, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [ tabnix.flakeModules.default ];

      perSystem =
        { pkgs, ... }:
        let
          cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);

          # The tabrules binary. Building it also runs `cargo test`, with the fixture tests.
          tabrules = pkgs.rustPlatform.buildRustPackage {
            pname = "tabrules";
            inherit (cargoToml.package) version;
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            # The just/doc-comment rule reads the recipes through `just`, and a fixture covers it.
            nativeCheckInputs = [ pkgs.just ];
            meta.mainProgram = "tabrules";
          };
        in
        {
          tabnix.packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
          ];
          tabnix.compiler = true;
          # lint-prose checks this repo with the binary that the same commit builds.
          tabnix.tabrules.package = tabrules;

          tabnix.hooks.rust = {
            name = "cargo fmt";
            entry = "${pkgs.writeShellScript "cargo-fmt-check" ''
              export PATH=${pkgs.cargo}/bin:${pkgs.rustfmt}/bin:$PATH
              exec cargo fmt --check
            ''}";
            files = "\\.rs$";
            pass_filenames = false;
          };

          packages.default = tabrules;

          checks.build = tabrules;
          checks.lint-rust = tabrules.overrideAttrs (old: {
            pname = "lint-rust";
            nativeBuildInputs = old.nativeBuildInputs ++ [
              pkgs.clippy
              pkgs.rustfmt
            ];
            buildPhase = ''
              cargo fmt --check
              cargo clippy --all-targets --locked --offline -- -D warnings
            '';
            doCheck = false;
            installPhase = "touch $out";
          });
        };
    };
}
