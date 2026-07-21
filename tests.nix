{
  runCommand,
  diffutils,
  stdenv,
  nixpkgs,
}:
let
  shared-config = {
    fileSystems."/".fsType = "tmpfs";
    boot.loader.grub.devices = [ "/dev/sda" ];
    system.stateVersion = "26.05";
  };

  etc-units = {
    reference = nixpkgs.lib.nixosSystem {
      inherit (stdenv.hostPlatform) system;
      modules = [ shared-config ];
    };
    test = nixpkgs.lib.nixosSystem {
      inherit (stdenv.hostPlatform) system;
      modules = [
        shared-config
        ./module.nix
        {
          nixosBuildHelpers.etc = true;
          nixosBuildHelpers.systemdUnits = true;
        }
      ];
    };
  };

  overlay = {
    reference = nixpkgs.lib.nixosSystem {
      inherit (stdenv.hostPlatform) system;
      modules = [
        shared-config
        {
          system.etc.overlay.enable = true;
        }
      ];
    };

    test = nixpkgs.lib.nixosSystem {
      inherit (stdenv.hostPlatform) system;
      modules = [
        ./module.nix
        shared-config
        {
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
        shared-config
        {
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
    set -e
    touch $out # otherwise build always fails

    echo "compares /etc and system-units (which are also placed inside /etc)"
    diff -r ${etc-units.reference.config.system.build.etc} ${etc-units.test.config.system.build.etc}

    echo "compares content of metadataImage"
    diff -r ${overlay.reference.config.system.build.etcMetadataImage} ${overlay.test.config.system.build.etcMetadataImage}

    echo "compares content of system path (/run/current-system/sw)"
    diff -r ${etc-units.reference.config.system.path} ${system-path.test.config.system.path}

    # Now eval tests the whole systems
    echo ${etc-units.reference.config.system.build.toplevel} \
      ${etc-units.test.config.system.build.toplevel} \
      ${overlay.reference.config.system.build.toplevel} \
      ${overlay.test.config.system.build.toplevel} \
      ${system-path.test.config.system.build.toplevel} \
  ''
