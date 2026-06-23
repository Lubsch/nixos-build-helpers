{
  outputs =
    { self, nixpkgs }:
    let
      mapSystems = f: builtins.mapAttrs f nixpkgs.legacyPackages;
    in
    {
      nixosModules.default = import ./module.nix;

      devShells = mapSystems (
        _: pkgs: {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
            ];
          };
        }
      );

    packages = mapSystems (
      _: pkgs: {
        default = pkgs.callPackage ./package.nix {};
      }
    );

    };
}
