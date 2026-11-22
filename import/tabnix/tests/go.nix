# The check of the go module of ADR-0700. A stub linter prints the config that the wrapper gives it.
{
  pkgs,
  inputs,
  module,
  go,
}:
let
  inherit (pkgs.stdenv.hostPlatform) system;

  stub = pkgs.writeShellScriptBin "golangci-lint" ''
    while [ "$#" -gt 0 ]; do
      if [ "$1" = --config ]; then
        cat "$2"
      fi
      shift
    done
  '';

  fixture =
    inputs.flake-parts.lib.mkFlake
      {
        inputs = inputs // {

          self = {
            outPath = ./fixtures/go;
            inherit inputs;
          };

        };
      }
      {
        systems = [ system ];
        imports = [
          module
          go
        ];
        perSystem =
          { config, ... }:
          {
            tabnix.go.package = stub;
            legacyPackages.fixture = { inherit (config.tabnix.go) golangci rules; };
          };
      };

  inherit (fixture.legacyPackages.${system}.fixture) golangci rules;
in
pkgs.runCommand "module-go" { } ''
  ${golangci}/bin/golangci-lint run --config ${./fixtures/go/golangci.yml} ./... > output
  grep -qF 'rules: "${rules}"' output
  if grep -qF '@tabnix-go-rules@' output; then
    exit 1
  fi
  grep -q noIfInitAssign ${rules}
  touch $out
''
