# The image-versions check of the default module, per ADR-0200. A Dockerfile ARG must name the version of the dev shell.
{
  pkgs,
  inputs,
  module,
}:
let
  inherit (pkgs.stdenv.hostPlatform) system;

  flake =
    want:
    inputs.flake-parts.lib.mkFlake
      {
        inputs = inputs // {

          self = {
            outPath = ./fixtures/image-versions;
            inherit inputs;
          };

        };
      }
      {
        systems = [ system ];
        imports = [ module ];
        perSystem = {
          tabkit.rules.enable = false;
          tabkit.imageVersions.GO_VERSION = want;
        };
      };

  same = (flake "1.0.0").checks.${system}.image-versions;
  other = pkgs.testers.testBuildFailure (flake "2.0.0").checks.${system}.image-versions;
in
pkgs.runCommand "image-versions" { } ''
  test -e ${same}
  grep -q 'GO_VERSION is 1.0.0, and the dev shell has 2.0.0' ${other}/testBuildFailure.log
  touch $out
''
