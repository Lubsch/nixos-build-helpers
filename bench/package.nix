# nixos-build-helpers of this checkout ("current") or of git revision `previousRev` ("previous")
{
  variant,
  previousRev ? null,
}:
let
  repo = toString ../.;
  pkgs = (builtins.getFlake repo).inputs.nixpkgs.legacyPackages.${builtins.currentSystem};
  src =
    if variant == "previous" then
      builtins.fetchGit {
        url = repo;
        rev = previousRev;
      }
    else
      ../.;
in
pkgs.callPackage (src + "/package.nix") { }
