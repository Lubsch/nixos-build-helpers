# nixos-build-helpers

This replaces the nixos builders for
- `{system,user,initrd,nspawn}-units` (bash)
- `etc` at `config.system.build.etc` (bash)
- `config.system.build.etcMetadataImage` (python)

## "Benchmarks"

TODO

## Installation

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

  nixosBuildHelpers = {
    etc = true;
    etcOverlay = true;
    systemdUnits = true;
  };
}
```
