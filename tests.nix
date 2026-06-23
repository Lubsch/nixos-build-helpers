{
  runCommand,
  diffutils,
  stdenv,
  nixpkgs,
}:
let
  reference = nixpkgs.lib.nixosSystem {
    inherit (stdenv.hostPlatform) system;
    modules = [
      {
        system.stateVersion = "26.05";
      }
    ];
  };

  test-etc-units = nixpkgs.lib.nixosSystem {
    inherit (stdenv.hostPlatform) system;
    modules = [
      (import ./module.nix)
      {
        nixosBuildHelpers.etc = true;
        nixosBuildHelpers.systemdUnits = true;
        system.stateVersion = "26.05";
      }
    ];
  };

  reference-overlay = nixpkgs.lib.nixosSystem {
    inherit (stdenv.hostPlatform) system;
    modules = [
      {
        system.stateVersion = "26.05";
        system.etc.overlay.enable = true;
      }
    ];
  };

  test-overlay = nixpkgs.lib.nixosSystem {
    inherit (stdenv.hostPlatform) system;
    modules = [
      (./module.nix)
      {
        system.stateVersion = "26.05";
        system.etc.overlay.enable = true;
        nixosBuildHelpers.etcOverlay = true;
      }
    ];
  };
in
runCommand "smoke-tests"
  {
    nativeBuildInputs = [ diffutils ];
  }
  ''
    touch $out # otherwise build always fails

    # compares /etc and system-units (which are also placed inside /etc)
    diff -r ${reference.config.system.build.etc} ${test-etc-units.config.system.build.etc}

    # compares content of metadataImage
    diff ${reference-overlay.config.system.build.etcMetadataImage} ${test-overlay.config.system.build.etcMetadataImage}
  ''
