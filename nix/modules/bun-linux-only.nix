# bun.nix lists the native packages of every platform, and a fetch downloads each one. Linux is the one supported
# platform, per ADR-0100, and Bun skips an optional package of another platform on install.
lib: bunNix:
let
  foreignPlatform =
    name:
    builtins.match ".*(^|[-/@])(darwin|win32|freebsd|openbsd|netbsd|sunos|android|aix)([-@].*|$)" name
    != null;
  original = import bunNix;
in
lib.setFunctionArgs (args: lib.filterAttrs (name: _: !foreignPlatform name) (original args)) (
  lib.functionArgs original
)
