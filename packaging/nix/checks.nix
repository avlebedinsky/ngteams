# Evaluation checks of the two modules. They build nothing: no check downloads the release.
{
  self,
  nixpkgs,
  pkgs,
}:
let
  inherit (pkgs) lib;
  system = pkgs.stdenv.hostPlatform.system;
  installs = packages: map (package: package.nixInstall or null) packages;
  check =
    name: ok: message:
    assert lib.assertMsg ok message;
    pkgs.runCommand "ngteams-${name}" { } "touch $out";

  nixos = nixpkgs.lib.nixosSystem {
    inherit system;
    modules = [
      self.nixosModules.default
      {
        programs.ngteams.enable = true;
        boot.loader.grub.enable = false;
        fileSystems."/" = {
          device = "none";
          fsType = "tmpfs";
        };
        system.stateVersion = lib.trivial.release;
      }
    ];
  };

  # Home Manager's own option for the packages, so that the module evaluates without a Home Manager input
  home = lib.evalModules {
    modules = [
      self.homeManagerModules.default
      {
        options.home.packages = lib.mkOption {
          type = lib.types.listOf lib.types.package;
          default = [ ];
        };
        config = {
          _module.args.pkgs = pkgs;
          programs.ngteams.enable = true;
        };
      }
    ];
  };

  # Home Manager as a NixOS module gives its modules `osConfig`
  homeInNixos = lib.evalModules {
    specialArgs.osConfig = { };
    modules = [
      self.homeManagerModules.default
      {
        options.home.packages = lib.mkOption {
          type = lib.types.listOf lib.types.package;
          default = [ ];
        };
        config = {
          _module.args.pkgs = pkgs;
          programs.ngteams.enable = true;
        };
      }
    ];
  };

  homeOff = lib.evalModules {
    modules = [
      self.homeManagerModules.default
      {
        options.home.packages = lib.mkOption {
          type = lib.types.listOf lib.types.package;
          default = [ ];
        };
        config._module.args.pkgs = pkgs;
      }
    ];
  };
in
{
  nixos-module = check "nixos-module" (
    builtins.elem "nixos" (installs nixos.config.environment.systemPackages)
    && nixos.config.hardware.graphics.enable
  ) "the NixOS module must install ngteams marked as a NixOS install and enable the graphics stack";

  home-manager-module =
    check "home-manager-module"
      (
        installs home.config.home.packages == [ "home-manager" ]
        && installs homeInNixos.config.home.packages == [ "nixos" ]
        && homeOff.config.home.packages == [ ]
      )
      "the Home Manager module must install ngteams marked as a Home Manager install (a NixOS install inside NixOS), and only when enabled";

  package =
    check "package"
      (
        self.packages.${system}.ngteams.nixInstall == "profile"
        && self.packages.${system}.ngteams.version == (lib.importJSON ./release.json).version
        && (pkgs.extend self.overlays.default).ngteams.nixInstall == "other"
        &&
          self.apps.${system}.default.program
          == lib.getExe (self.packages.${system}.ngteams.override { nixInstall = "other"; })
        && self.apps.${system}.ngteams.program == self.apps.${system}.default.program
      )
      "the flake's package must be the release of release.json, marked as a profile install; nix run and the overlay name no install";
}
