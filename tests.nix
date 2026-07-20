{
  runCommand,
  diffutils,
  stdenv,
  nixpkgs,
}:
let
  etc-units = {
    reference = nixpkgs.lib.nixosSystem {
      inherit (stdenv.hostPlatform) system;
      modules = [
        {
          system.stateVersion = "26.05";
        }
      ];
    };
    test = nixpkgs.lib.nixosSystem {
      inherit (stdenv.hostPlatform) system;
      modules = [
        ./module.nix
        {
          nixosBuildHelpers.etc = true;
          nixosBuildHelpers.systemdUnits = true;
          system.stateVersion = "26.05";
        }
      ];
    };
  };

  overlay = {
    reference = nixpkgs.lib.nixosSystem {
      inherit (stdenv.hostPlatform) system;
      modules = [
        {
          system.stateVersion = "26.05";
          system.etc.overlay.enable = true;
        }
      ];
    };

    test = nixpkgs.lib.nixosSystem {
      inherit (stdenv.hostPlatform) system;
      modules = [
        ./module.nix
        {
          system.stateVersion = "26.05";
          system.etc.overlay.enable = true;
          nixosBuildHelpers.etcOverlay = true;
        }
      ];
    };
  };

  system-path = {
    test = nixpkgs.lib.nixosSystem {
      inherit (stdenv.hostPlatform) system;
      modules = [
        ./module.nix
        {
          system.stateVersion = "26.05";
          nixosBuildHelpers.buildEnv = true;
        }
      ];
    };
  };

in
runCommand "smoke-tests"
  {
    nativeBuildInputs = [ diffutils ];
    passthru = {
      inherit etc-units overlay system-path;
    };
  }
  ''
    touch $out # otherwise build always fails

    echo "compares /etc and system-units (which are also placed inside /etc)"
    diff -r ${etc-units.reference.config.system.build.etc} ${etc-units.test.config.system.build.etc}

    echo "compares content of metadataImage"
    diff ${overlay.reference.config.system.build.etcMetadataImage} ${overlay.test.config.system.build.etcMetadataImage}

    echo "compares content of system path (/run/current-system/sw)"
    diff ${etc-units.reference.config.system.path} ${system-path.test.config.system.path}
  ''
