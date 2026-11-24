# Differential tests: build each output with nixpkgs' builder and with ours, and compare.
#
# `checks` is what `nix flake check` runs. `matrix` additionally covers every combination of
# options on the featureful configuration (nix build .#tests.matrix).
{ pkgs, nixpkgs }:
let
  inherit (pkgs) lib;
  fixtures = pkgs.callPackage ./fixtures.nix { };
  configs = import ./configs.nix { inherit fixtures; };
  compare = "${lib.getExe pkgs.python3} ${./compare-trees.py}";
  builderOptions = {
    etc = false;
    etcOverlay = false;
    systemdUnits = false;
    buildEnv = false;
  };
  noOptions = builderOptions // {
    fixDependencies = false;
  };

  mkSystem =
    {
      config,
      overlay,
      options,
    }:
    nixpkgs.lib.nixosSystem {
      specialArgs = { inherit overlay; };
      modules = [
        config
        ../module.nix
        {
          nixpkgs.pkgs = pkgs;
          nixosBuildHelpers = noOptions // options;
        }
      ];
    };

  # The same system built by nixpkgs. fixDependencies changes what's built, not how, so it's kept.
  referenceOf =
    system:
    (system.extendModules {
      modules = [
        { nixosBuildHelpers = lib.mkForce (system.config.nixosBuildHelpers // builderOptions); }
      ];
    }).config;

  composefsDump =
    image:
    pkgs.runCommand "etc-dump" {
      nativeBuildInputs = [ pkgs.composefs ];
    } "composefs-info dump ${image} > $out";

  # The outputs our builders replace, plus their inputs as built by our other builders.
  components =
    c:
    let
      etc = c.environment.etc;
      units = {
        system-units = etc."systemd/system".source;
        user-units = etc."systemd/user".source;
        initrd-units = c.boot.initrd.systemd.contents."/etc/systemd/system".source or null;
        nspawn-units = etc."systemd/nspawn".source or null;
      };
    in
    lib.filterAttrs (_: v: v.drv != null) (
      lib.mapAttrs (_: drv: {
        inherit drv;
        inputs = [ "buildEnv" ];
      }) units
      // {
        system-path = {
          drv = c.system.path;
          inputs = [ ];
        };
        etc = {
          drv = c.system.build.etc;
          inputs = [
            "systemdUnits"
            "buildEnv"
          ];
        };
        etc-metadata = {
          drv = if c.system.etc.overlay.enable then composefsDump c.system.build.etcMetadataImage else null;
          inputs = [
            "systemdUnits"
            "buildEnv"
          ];
        };
        # Everything else, e.g. the activation script and $out/etc, which switch-to-configuration reads
        toplevel = {
          drv = c.system.build.toplevel;
          # contains nixos-build-helpers, for offline rebuilds
          ignore = [ "extra-dependencies" ];
          inputs = [
            "etc"
            "etcOverlay"
            "systemdUnits"
            "buildEnv"
          ];
        };
      }
    );

  compareSystems =
    name: system:
    let
      test = system.config;
      ref = referenceOf system;
      opts = test.nixosBuildHelpers;
      refComponents = components ref;
      lines = lib.mapAttrsToList (
        component:
        {
          drv,
          inputs,
          ignore ? [ ],
        }:
        let
          normalize = lib.any (o: opts.${o}) inputs;
          flags =
            lib.optional normalize "--normalize"
            ++ lib.concatMap (i: [
              "--ignore"
              i
            ]) ignore;
        in
        ''
          echo "== ${component}${lib.optionalString normalize " (store hashes normalized)"}"
          ${compare} ${toString flags} ${refComponents.${component}.drv} ${drv} || failed=1
        ''
      ) (components test);
    in
    pkgs.runCommand "test-${name}" { } ''
      failed=0
      ${lib.concatStrings lines}
      ${lib.optionalString opts.etcOverlay ''
        ${lib.optionalString (!opts.systemdUnits && !opts.buildEnv) ''
          echo "== etc-metadata.erofs (bytes)"
          cmp ${ref.system.build.etcMetadataImage} ${test.system.build.etcMetadataImage} || failed=1
        ''}
      ''}
      [ $failed = 0 ] && touch $out
    '';

  mkTest =
    config: overlay: options:
    compareSystems
      "${config}${lib.optionalString overlay "-overlay"}-${
        lib.concatStringsSep "-" (lib.attrNames (lib.filterAttrs (_: v: v) options))
      }"
      (mkSystem {
        config = configs.${config};
        inherit overlay options;
      });

  all = {
    systemdUnits = true;
    buildEnv = true;
    fixDependencies = true;
  };

  # fixDependencies changes /etc by design. Check that it only changes what it should.
  fixDependencies =
    let
      test = mkSystem {
        config = configs.featureful;
        overlay = false;
        options.fixDependencies = true;
      };
      ref = mkSystem {
        config = configs.featureful;
        overlay = false;
        options = { };
      };
      c = test.config;
      expectedDbus = map (
        p: if p == ref.config.system.path then "/run/current-system/sw" else toString p
      ) ref.config.services.dbus.packages;
      check = msg: cond: lib.optionalString (!cond) "echo 'FAIL: ${msg}'; failed=1\n";
    in
    pkgs.runCommand "test-fix-dependencies" { } ''
      failed=0
      ${check "dbus packages are upstream's with system.path replaced" (
        map toString c.services.dbus.packages == expectedDbus
      )}
      ${check "dbus packages that aren't in systemPackages are kept" (
        lib.elem fixtures.dbusOnly c.services.dbus.packages
      )}
      ${check "terminfo points at /run/current-system/sw" (
        c.environment.etc.terminfo.source == "/run/current-system/sw/share/terminfo"
      )}
      [ $failed = 0 ] && touch $out
    '';

  checks = {
    featureful-etc = mkTest "featureful" false { etc = true; };
    featureful-etcOverlay = mkTest "featureful" true { etcOverlay = true; };
    featureful-systemdUnits = mkTest "featureful" false { systemdUnits = true; };
    featureful-buildEnv = mkTest "featureful" false { buildEnv = true; };
    featureful-fixDependencies = mkTest "featureful" false { fixDependencies = true; };
    featureful-all = mkTest "featureful" false (all // { etc = true; });
    featureful-overlay-all = mkTest "featureful" true (all // { etcOverlay = true; });
    minimal-all = mkTest "minimal" false (all // { etc = true; });
    minimal-overlay-all = mkTest "minimal" true (all // { etcOverlay = true; });
    fix-dependencies = fixDependencies;
    build-env = pkgs.callPackage ./build-env.nix { inherit fixtures compare; };
    upstream = pkgs.callPackage ./upstream.nix { };
    vm = pkgs.callPackage ./vm.nix { overlay = false; };
    vm-overlay = pkgs.callPackage ./vm.nix { overlay = true; };
  };

  # Every combination of options (etc and etcOverlay are exclusive).
  matrix =
    let
      bools = [
        false
        true
      ];
      combos = lib.cartesianProduct {
        etcMode = [
          null
          "etc"
          "etcOverlay"
        ];
        systemdUnits = bools;
        buildEnv = bools;
        fixDependencies = bools;
      };
    in
    pkgs.linkFarm "test-matrix" (
      map (
        { etcMode, ... }@o:
        let
          drv = mkTest "featureful" (etcMode == "etcOverlay") (
            removeAttrs o [ "etcMode" ] // lib.optionalAttrs (etcMode != null) { ${etcMode} = true; }
          );
        in
        {
          inherit (drv) name;
          path = drv;
        }
      ) combos
    );
in
{
  inherit checks matrix mkSystem;
}
