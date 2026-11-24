{
  outputs =
    { self, nixpkgs }:
    let
      forAllSystems =
        f: nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (s: f nixpkgs.legacyPackages.${s});
      tests = pkgs: import ./tests { inherit pkgs nixpkgs; };
    in
    {
      nixosModules.default = ./module.nix;

      packages = forAllSystems (pkgs: {
        default = pkgs.callPackage ./package.nix { };
      });

      checks = forAllSystems (
        pkgs: (tests pkgs).checks // { package = self.packages.${pkgs.stdenv.hostPlatform.system}.default; }
      );

      # nix build .#tests.matrix
      legacyPackages = forAllSystems (pkgs: {
        tests = tests pkgs;
      });

      apps = forAllSystems (pkgs: {
        # nix run .#check-deps -- <flake>#nixosConfigurations.<name>
        check-deps = {
          type = "app";
          meta.description = "Check that /etc of a NixOS system doesn't depend on system.path";
          program = toString (
            pkgs.writeShellScript "check-deps" ''
              exec ${pkgs.python3}/bin/python3 ${./bench/check_deps.py} "$@"
            ''
          );
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            python3
          ];
        };
      });
    };
}
