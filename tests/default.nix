# Differential tests: build each output with nixpkgs' builder and with ours, and compare.
#
# `checks` is what `nix flake check` runs. `matrix` additionally covers every combination of
# options on the featureful configuration (nix build .#tests.matrix, 48 systems).
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
    fewerDerivations = false;
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
      fewer = c.nixosBuildHelpers.fewerDerivations;
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
        inputs = [
          "buildEnv"
          "fewerDerivations"
        ];
        # Units link the same files, but from one output instead of a derivation each.
        deref = fewer && c.nixosBuildHelpers.systemdUnits;
      }) units
      // {
        system-path = {
          # With fixDependencies, config.system.path is /run/current-system/sw
          drv = c.system.build.systemPath or c.system.path;
          inputs = [ ];
        };
        etc = {
          drv = c.system.build.etc;
          inputs = [
            "systemdUnits"
            "buildEnv"
            "fewerDerivations"
          ];
        };
        etc-metadata = {
          drv = if c.system.etc.overlay.enable then composefsDump c.system.build.etcMetadataImage else null;
          inputs = [
            "systemdUnits"
            "buildEnv"
            "fewerDerivations"
          ];
        };
        # Everything else, e.g. the activation script and $out/etc, which switch-to-configuration reads
        toplevel = {
          drv = c.system.build.toplevel;
          # extra-dependencies contains nixos-build-helpers, for offline rebuilds. fewerDerivations
          # adds links for the activation script to find /etc, which upstream has in the script.
          ignore = [
            "extra-dependencies"
          ]
          ++ lib.optionals fewer [
            "etc-metadata-image"
            "etc-basedir"
          ];
          subst = lib.optionals fewer [
            [
              ''$(readlink -f "$systemConfig/etc")''
              "/nix/store/<hash>-etc/etc"
            ]
            [
              ''$(readlink -f "$systemConfig/etc-metadata-image")''
              "/nix/store/<hash>-etc-metadata.erofs"
            ]
            [
              ''$(readlink -f "$systemConfig/etc-basedir")''
              "/nix/store/<hash>-etc-lowerdir"
            ]
          ];
          inputs = lib.attrNames builderOptions;
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
          subst ? [ ],
          deref ? false,
        }:
        let
          normalize = lib.any (o: opts.${o}) inputs;
          flags =
            lib.optional normalize "--normalize"
            ++ lib.optional deref "--deref"
            ++ lib.concatMap (i: [
              "--ignore"
              i
            ]) ignore
            ++ lib.concatMap (fromTo: [
              "--subst"
              (lib.escapeShellArg (lib.concatStringsSep "=" fromTo))
            ]) subst;
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
    fewerDerivations = true;
    systemdUnits = true;
    buildEnv = true;
    fixDependencies = true;
  };

  # fixDependencies changes /etc by design. Check that it only changes what it should.
  fixDependencies =
    let
      # Modules that refer to system.path at run time
      users = {
        security.polkit.enable = true;
        services.accounts-daemon.enable = true;
        services.cron = {
          enable = true;
          systemCronJobs = [ "* * * * * root true" ];
        };
      };
      mk =
        options:
        mkSystem {
          config = {
            imports = [
              configs.featureful
              users
            ];
          };
          overlay = false;
          inherit options;
        };
      test = mk { fixDependencies = true; };
      ref = mk { };
      c = test.config;
      sw = "/run/current-system/sw";
      expectedDbus = map (
        p: if p == ref.config.system.path then sw else toString p
      ) ref.config.services.dbus.packages;
      noContext = s: !builtins.hasContext s;
      # Everything else must be upstream's, with system.path replaced by ${sw}: a module that reads
      # system.path at build time and quietly skips what it can't find would show up here.
      refComponents = components ref.config;
      compareWithUpstream = lib.concatStrings (
        lib.mapAttrsToList (name: { drv, ... }: ''
          echo "== ${name} (vs upstream, files behind store links compared)"
          ${compare} --normalize --deref --ignore sw --ignore extra-dependencies \
            --subst-ref '/nix/store/<hash>-system-path=${sw}' \
            ${refComponents.${name}.drv} ${drv} || failed=1
        '') (removeAttrs (components c) [ "system-path" ])
      );
      check = msg: cond: lib.optionalString (!cond) "echo 'FAIL: ${msg}'; failed=1\n";
    in
    pkgs.runCommand "test-fix-dependencies" { } ''
      failed=0
      ${check "system.path is ${sw}, without context" (
        "${c.system.path}" == sw && noContext "${c.system.path}"
      )}
      ${check "system.build.systemPath is upstream's system.path" (
        c.system.build.systemPath.drvPath == ref.config.system.path.drvPath
      )}
      ${check "dbus packages are upstream's with system.path replaced" (
        map toString c.services.dbus.packages == expectedDbus
      )}
      ${check "dbus packages that aren't in systemPackages are kept" (
        lib.elem fixtures.dbusOnly c.services.dbus.packages
      )}
      ${check "terminfo points at ${sw}" (c.environment.etc.terminfo.source == "${sw}/share/terminfo")}
      ${check "polkit's restartTriggers are constant" (
        map toString c.systemd.services.polkit.restartTriggers == [ sw ]
      )}
      ${check "accounts-daemon finds its interfaces through ${sw}" (
        c.systemd.services.accounts-daemon.environment.XDG_DATA_DIRS == "${sw}/share"
      )}
      grep -q '^PATH=${sw}/bin:${sw}/sbin$' ${c.environment.etc.crontab.source} \
        || { echo "FAIL: the crontab's PATH is ${sw}"; failed=1; }
      # The toplevel links the real system.path, which is all that refers to it
      [[ $(readlink ${c.system.build.toplevel}/sw) == ${ref.config.system.path} ]] \
        || { echo "FAIL: the toplevel's sw is upstream's system.path"; failed=1; }
      ${compareWithUpstream}
      [ $failed = 0 ] && touch $out
    '';

  checks = {
    featureful-etc = mkTest "featureful" false { etc = true; };
    featureful-etcOverlay = mkTest "featureful" true { etcOverlay = true; };
    featureful-systemdUnits = mkTest "featureful" false { systemdUnits = true; };
    featureful-buildEnv = mkTest "featureful" false { buildEnv = true; };
    featureful-fixDependencies = mkTest "featureful" false { fixDependencies = true; };
    featureful-fewerDerivations = mkTest "featureful" false { fewerDerivations = true; };
    featureful-overlay-fewerDerivations = mkTest "featureful" true { fewerDerivations = true; };
    featureful-fewerDerivations-systemdUnits = mkTest "featureful" false {
      fewerDerivations = true;
      systemdUnits = true;
    };
    featureful-all = mkTest "featureful" false (all // { etc = true; });
    featureful-overlay-all = mkTest "featureful" true (all // { etcOverlay = true; });
    minimal-all = mkTest "minimal" false (all // { etc = true; });
    minimal-overlay-all = mkTest "minimal" true (all // { etcOverlay = true; });
    fix-dependencies = fixDependencies;
    build-env = pkgs.callPackage ./build-env.nix { inherit fixtures compare; };
    # buildEnv's install-info, on the Info files of the featureful system and generated ones
    install-info =
      let
        corpus =
          (mkSystem {
            config = configs.featureful;
            overlay = false;
            options = { };
          }).config.system.path;
      in
      pkgs.runCommand "test-install-info" { } ''
        ${lib.getExe pkgs.python3} ${./install-info.py} ${pkgs.buildPackages.texinfo}/bin/install-info \
          ${lib.getExe (pkgs.callPackage ../package.nix { })} ${corpus}/share/info 400
        touch $out
      '';
    upstream = pkgs.callPackage ./upstream.nix { };
    vm = pkgs.callPackage ./vm.nix { overlay = false; };
    vm-overlay = pkgs.callPackage ./vm.nix { overlay = true; };
    systemd-boot = pkgs.callPackage ./systemd-boot.nix { };
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
        fewerDerivations = bools;
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
