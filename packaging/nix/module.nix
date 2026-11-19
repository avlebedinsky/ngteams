# programs.ngteams for NixOS (`kind = "nixos"`) and for Home Manager (`kind = "home-manager"`).
#
# The package is given its `nixInstall`, so that the app's update notice names the right command. A package of
# another origin (one without that argument) is installed as it is.
{ self, kind }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.ngteams;
  system = pkgs.stdenv.hostPlatform.system;
  package =
    if cfg.package ? nixInstall then cfg.package.override { nixInstall = kind; } else cfg.package;
in
{
  options.programs.ngteams = {
    enable = lib.mkEnableOption "ngteams, a native Microsoft Teams client";
    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${system}.ngteams;
      defaultText = lib.literalExpression "ngteams.packages.\${pkgs.stdenv.hostPlatform.system}.ngteams";
      description = "The ngteams package to install.";
    };
  };

  config = lib.mkIf cfg.enable (
    if kind == "nixos" then
      {
        environment.systemPackages = [ package ];
        # the app draws with the GPU (Vulkan or OpenGL)
        hardware.graphics.enable = lib.mkDefault true;
      }
    else
      { home.packages = [ package ]; }
  );
}
