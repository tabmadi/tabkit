# The check of the template module of ADR-0400. A fixture flake imports it, and the check asserts what it outputs.
{
  pkgs,
  inputs,
  template,
}:
let
  inherit (pkgs.stdenv.hostPlatform) system;

  # A flake made from a fixture folder, with the template module.
  fixture =
    path:
    inputs.flake-parts.lib.mkFlake
      {
        inputs = inputs // {

          self = {
            outPath = path;
            inherit inputs;
          };

        };
      }
      {
        systems = [ system ];
        imports = [ template ];
        tabkit.template = {
          description = "A fixture";
          welcomeText = "The first steps";
          exclude = [ "infra" ];
          variants.self-hosted.welcomeText = "The first steps of a host";
        };
      };

  good = fixture ./fixtures/template;
  voice = fixture ./fixtures/template-voice;
in
pkgs.runCommand "module-template"
  {
    # A doc that speaks as the template must fail lint-scope.
    refused = pkgs.testers.testBuildFailure voice.checks.${system}.lint-scope;
  }
  ''
    test -e ${good.checks.${system}.lint-scope}
    test -f ${good.templates.default.path}/AGENTS.md
    test ! -e ${good.templates.default.path}/template
    test "${good.templates.default.description}" = "A fixture"
    test ! -e ${good.templates.default.path}/infra
    test -f ${good.templates.self-hosted.path}/infra/README.md
    test ! -e ${good.templates.self-hosted.path}/template
    test "${good.templates.self-hosted.welcomeText}" = "The first steps of a host"
    grep -q "speaks as the template" $refused/testBuildFailure.log
    touch $out
  ''
