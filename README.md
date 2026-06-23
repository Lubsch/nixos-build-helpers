# Installation

Add the flake as an input:
```nix
inputs = {
  ...
  nixos-build-helpers = {
    url = "github:lubsch/nixos-build-helpers";
    inputs.nixpkgs.follows = "nixpkgs";
  };
};
```

Add and enable the module:
```nix
{ inputs, ... }:
{
  imports = [ inputs.nixos-build-helpers.nixosModules.default ];

  systemBuildHelper = {
    etc = true;
    etcOverlay = true;
    systemdUnits = true;
  };
}
```
