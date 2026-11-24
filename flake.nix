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
        # nix run .#bench -- --help
        bench = {
          type = "app";
          meta.description = "Benchmark nixos-build-helpers against nixpkgs";
          program = toString (
            pkgs.writeShellScript "bench" ''
              # A pinned nix client, so that timings don't depend on what's installed
              export PATH=${
                pkgs.lib.makeBinPath [
                  pkgs.nix
                  pkgs.git
                  pkgs.hyperfine
                ]
              }:$PATH
              # Run from the checkout: it needs git history (--previous-rev) and writes results
              exec ${pkgs.python3}/bin/python3 "$(git rev-parse --show-toplevel)/bench/bench.py" "$@"
            ''
          );
        };
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
