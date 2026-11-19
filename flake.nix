{
  description = "ngteams: a native Microsoft Teams client for Linux";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      inherit (nixpkgs) lib;
      # the systems that have a release archive (packaging/nix/release.json, which CI writes at each release)
      systems = builtins.filter (name: name != "version") (
        builtins.attrNames (lib.importJSON ./packaging/nix/release.json)
      );
      forSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      # `nix profile install` takes one of these packages; the other outputs give a copy that names no command
      packages = forSystems (pkgs: rec {
        ngteams = pkgs.callPackage ./packaging/nix/package.nix { nixInstall = "profile"; };
        default = ngteams;
      });

      # `nix run .#ngteams` takes `apps.ngteams` before `packages.ngteams`, so both forms run the copy that names no command
      apps = forSystems (pkgs: rec {
        ngteams = {
          type = "app";
          program = lib.getExe (
            self.packages.${pkgs.stdenv.hostPlatform.system}.ngteams.override { nixInstall = "other"; }
          );
          meta.description = "Run ngteams";
        };
        default = ngteams;
      });

      overlays.default = final: _prev: { ngteams = final.callPackage ./packaging/nix/package.nix { }; };

      nixosModules.default = import ./packaging/nix/module.nix {
        inherit self;
        kind = "nixos";
      };

      homeManagerModules.default = import ./packaging/nix/module.nix {
        inherit self;
        kind = "home-manager";
      };

      checks = forSystems (pkgs: import ./packaging/nix/checks.nix { inherit self nixpkgs pkgs; });

      formatter = forSystems (pkgs: pkgs.nixfmt);
    };
}
