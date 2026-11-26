{
  config,
  lib,
  pkgs,
  options,
  modulesPath,
  extendModules,
  ...
}:
let
  inherit (lib) mkIf mkForce mkEnableOption;
  cfg = config.nixosBuildHelpers;
  nixos-build-helpers = pkgs.buildPackages.callPackage ./package.nix { };
  exe = lib.getExe nixos-build-helpers;

  # This system without any of our overrides. It shares `pkgs`, so evaluating values from it is
  # cheap. Wrapping upstream's values instead of copying their definitions keeps us in sync with
  # nixpkgs, e.g. system.path's postBuild or modules adding to services.dbus.packages.
  upstreamSystem = extendModules {
    modules = [
      # Stronger than mkForce, so that this also works on variants that mkForce our options.
      (lib.mkOverride 1 {
        nixosBuildHelpers = lib.mapAttrs (_: _: false) cfg;
        nixpkgs.pkgs = pkgs;
      })
    ];
  };
  upstream = upstreamSystem.config;

  # Comparing derivations (==) would evaluate upstream's system.path just for that (~0.2 s).
  isSystemPath = p: lib.isDerivation p && p.name == upstream.system.path.name;

  # Only the attributes our builders read. Passing whole entries would put e.g. every `text` into
  # the derivation a second time.
  etcEntries = map (f: {
    source = "${f.source}"; # like upstream: forces local paths into the store
    inherit (f)
      target
      mode
      user
      group
      uid
      gid
      ;
  }) (lib.filter (f: f.enable) (lib.attrValues config.environment.etc));

  # /etc, and with etcOverlay its metadata image as a second output: they're built from the same
  # inputs, so one derivation saves scheduling one. Not stdenv, because its setup can't handle
  # the output name, which gives the image upstream's name: <hash>-etc-metadata.erofs.
  etc =
    if cfg.etcOverlay then
      derivation {
        name = "etc";
        inherit (pkgs.stdenv.buildPlatform) system;
        outputs = [
          "out"
          "metadata.erofs"
        ];
        __structuredAttrs = true;
        preferLocalBuild = true;
        allowSubstitutes = false;
        entries = etcEntries;
        builder = "${pkgs.buildPackages.bash}/bin/bash";
        args = [
          "-euc"
          (with pkgs.buildPackages; ''
            source "$NIX_ATTRS_SH_FILE"
            image=''${outputs[metadata.erofs]}
            out=''${outputs[out]} ${exe} build-etc
            ${exe} build-composefs-dump > etc-dump
            ${composefs}/bin/mkcomposefs --from-file etc-dump "$image"
            ${erofs-utils}/bin/fsck.erofs "$image"
          '')
        ];
      }
    else
      pkgs.runCommandLocal "etc" {
        __structuredAttrs = true;
        entries = etcEntries;
      } "${exe} build-etc";

  upstreamUtils = import "${modulesPath}/../lib/utils.nix" { inherit lib config pkgs; };
  upstreamGenerateUnits = upstreamUtils.systemdUtils.lib.generateUnits;

  # Same signature as upstream (checked in `assertions`).
  generateUnits =
    {
      allowCollisions ? true,
      type,
      units,
      upstreamUnits,
      upstreamWants,
      packages ? config.systemd.packages,
      package ? config.systemd.package,
    }:
    pkgs.runCommand "${type}-units" {
      __structuredAttrs = true;
      preferLocalBuild = true;
      allowSubstitutes = false;
      outputs = [ "out" ] ++ lib.optional cfg.fewerDerivations "files";
      inherit
        allowCollisions
        type
        upstreamUnits
        upstreamWants
        packages
        package
        ;
      inherit (config.systemd) defaultUnit ctrlAltDelUnit;
      units = lib.mapAttrs (
        name: u:
        let
          inline = cfg.fewerDerivations && isMadeFromText name u;
        in
        {
          # With fewerDerivations, the unit files are written to the `files` output instead of
          # one derivation each, unless their derivation was replaced.
          unit = if inline then null else "${u.unit}";
          text = if inline && u.enable then u.text or "" else null;
          overrideStrategy = u.overrideStrategy or "asDropinIfExists";
          aliases = u.aliases or [ ];
          wantedBy = u.wantedBy or [ ];
          upheldBy = u.upheldBy or [ ];
          requiredBy = u.requiredBy or [ ];
        }
      ) units;
    } "${exe} generate-units";

  # Whether a unit's derivation is still upstream's makeUnit of its text.
  isMadeFromText =
    name: u:
    let
      drvName = "unit-${upstreamUtils.systemdUtils.lib.mkPathSafeName name}";
    in
    if u.enable then
      u.unit.name or null == drvName && u.unit.text or null == u.text or ""
    else
      u.unit.name or null == "${drvName}-disabled";

  overlay = config.system.etc.overlay.enable;

  # With fewerDerivations, the activation script finds /etc through the toplevel at run time, so
  # that it doesn't depend on (and isn't rebuilt with) /etc. Resolved to the store paths
  # upstream's script uses, e.g. to compare with /run/current-system/etc.
  atActivation = {
    etc = ''$(readlink -f "$systemConfig/etc")'';
    image = ''$(readlink -f "$systemConfig/etc-metadata-image")'';
    basedir = ''$(readlink -f "$systemConfig/etc-basedir")'';
  };

  # Upstream's overlay activation snippet with the paths of /etc replaced. It's taken from the
  # system without our options, with placeholders for those paths, so that neither upstream's
  # /etc (all its builders) has to be evaluated, nor the snippet copied.
  overlayActivationCommands =
    let
      placeholders = {
        etc = "@nixosBuildHelpers-etc@";
        image = "@nixosBuildHelpers-etc-metadata-image@";
        basedir = "@nixosBuildHelpers-etc-basedir@";
      };
      upstreamCommands =
        (upstreamSystem.extendModules {
          modules = [
            {
              system.build = {
                etc = mkForce { outPath = placeholders.etc; };
                etcMetadataImage = mkForce placeholders.image;
                etcBasedir = mkForce placeholders.basedir;
              };
            }
          ];
        }).config.system.build.etcActivationCommands;
    in
    # The image is all that the remount needs (e.g. nixpkgs#533343 removes the comparison).
    assert lib.assertMsg (lib.hasInfix placeholders.image upstreamCommands)
      "nixosBuildHelpers.fewerDerivations: nixpkgs' etc activation commands changed. nixos-build-helpers needs an update.";
    builtins.replaceStrings
      [
        "${placeholders.etc}/etc"
        placeholders.image
        placeholders.basedir
      ]
      [
        atActivation.etc
        atActivation.image
        atActivation.basedir
      ]
      upstreamCommands;

  # Text that refers to no store paths can be written at evaluation time, without a derivation.
  # (Not if it mentions store paths without context: a derivation would find those references.)
  isPlainText = text: !builtins.hasContext text && !lib.hasInfix builtins.storeDir text;

  # switch-to-configuration with ./patches/switch-to-configuration.patch (see the README).
  upstreamSwitchToConfiguration = lib.getExe pkgs.switch-to-configuration-ng;
  switchToConfiguration = pkgs.switch-to-configuration-ng.overrideAttrs (old: {
    patches = old.patches or [ ] ++ [ ./patches/switch-to-configuration.patch ];
  });

  # Upstream's systemd-boot installer with ./patches/systemd-boot-builder.patch applied to its
  # builder (see the README). The builder is the template with this system's values filled in, so
  # it's patched at build time, where upstream's installer names it: the patch's context has none
  # of those values. Run byte-compiled, without `site` (stdlib only), from a second output.
  systemdBootInstaller = pkgs.runCommand "install-systemd-boot.sh" {
    preferLocalBuild = true;
    allowSubstitutes = false;
    outputs = [
      "out"
      "lib"
    ];
    upstreamInstaller = upstream.system.build.installBootLoader;
    nativeBuildInputs = [ pkgs.buildPackages.patch ];
  } ''
    fail() {
      echo "nixosBuildHelpers.systemdBoot: $1. nixos-build-helpers needs an update." >&2
      exit 1
    }
    upstreamBuilder=$(grep -o '${builtins.storeDir}/[a-z0-9]*-systemd-boot/bin/systemd-boot' "$upstreamInstaller") \
      || fail "upstream's installer doesn't run a systemd-boot builder"
    python=$(sed -n '1s|^#! *\(${builtins.storeDir}/[^ ]*/bin/python3[^ ]*\) -B$|\1|p' "$upstreamBuilder")
    [[ -n $python ]] || fail "the builder's interpreter line changed"

    cp "$upstreamBuilder" systemd-boot-builder.py
    chmod u+w systemd-boot-builder.py
    patch -p1 --forward --no-backup-if-mismatch < ${./patches/systemd-boot-builder.patch} \
      || fail "the patch doesn't apply to the builder"

    mkdir "$lib"
    if "$python" -c pass 2>/dev/null; then
      "$python" -c 'import py_compile, sys; py_compile.compile("systemd-boot-builder.py", cfile=sys.argv[1], doraise=True, invalidation_mode=py_compile.PycInvalidationMode.UNCHECKED_HASH)' "$lib/systemd-boot.pyc"
      run="$python -BS $lib/systemd-boot.pyc"
    else # cross-compiled: the host's python can't run here
      cp systemd-boot-builder.py "$lib/systemd-boot.py"
      run="$python -BS $lib/systemd-boot.py"
    fi
    grep -q "^$upstreamBuilder \"\$@\"$" "$upstreamInstaller" || fail "upstream's installer runs its builder differently"
    sed "s|^$upstreamBuilder \"\$@\"$|$run \"\$@\"|" "$upstreamInstaller" > "$out"
    chmod +x "$out"
  '';

  # The shape of upstream's build command for buildEnv, which we replace.
  upstreamBuildEnvCommand = builtins.unsafeDiscardStringContext "${pkgs.buildPackages.perl}/bin/perl -w ${builtins.storeDir}/[a-z0-9]{32}-builder.pl\neval \"\\$postBuild\"\n";
in
{
  options.nixosBuildHelpers = {
    etc = mkEnableOption "the faster builder for `system.build.etc`";
    etcOverlay = mkEnableOption "the faster builder for `system.build.etcMetadataImage` (used with `system.etc.overlay`). Implies `etc`";
    systemdUnits = mkEnableOption "the faster builder for systemd units (system-units, user-units, initrd-units, nspawn-units)";
    buildEnv = mkEnableOption "the faster builder for `system.path`";
    fixDependencies = mkEnableOption "removing the dependency of /etc on `system.path`, so they build in parallel and /etc isn't rebuilt when only packages change";
    switchToConfiguration = mkEnableOption "a patched `switch-to-configuration` that waits less: 10 ms instead of 250 ms for systemd's events to settle, and the user managers are only reexecuted when their systemd or user.conf changes, and only reloaded when something on their unit search path, their generators or what those read changed with the toplevel, and the unit files of the active units are looked up in one batch of D-Bus calls instead of one call after the other. Unlike the other options, this changes behaviour (see the README)";
    systemdBoot = mkEnableOption "a patched systemd-boot installer that starts fewer processes (no nix-env, check-mountpoints or an empty copy-extra-files script; bootctl only when the systemd package changes) and runs byte-compiled. Unlike the other options, this changes behaviour (see the README)";
    fewerDerivations = mkEnableOption "fewer derivations on the way from a change to the toplevel: `environment.etc.*.text` that refers to no store paths is written at evaluation time (`builtins.toFile`), the activation script and boot.json don't depend on /etc, and with `systemdUnits`, units are written by the units' derivation instead of one derivation each";
  };

  # Extend the submodule of environment.etc with a stronger definition of `source`.
  options.environment.etc = lib.mkOption {
    type = lib.types.attrsOf (
      lib.types.submodule (
        {
          name,
          config,
          options,
          ...
        }:
        {
          config.source = mkIf (cfg.fewerDerivations && config.text != null && isPlainText config.text) (
            # upstream: mkDerivedConfig options.text (pkgs.writeText name')
            lib.mkOverride ((options.text.highestPrio or lib.modules.defaultOverridePriority) - 1) (
              builtins.toFile ("etc-" + lib.replaceStrings [ "/" ] [ "-" ] name) config.text
            )
          );
        }
      )
    );
  };

  config = lib.mkMerge [
    (mkIf (lib.any lib.id (lib.attrValues cfg)) {
      # Keep the builder in the store, so that offline rebuilds work after garbage collection.
      system.extraDependencies = [ nixos-build-helpers ];
    })

    (mkIf (cfg.etc || cfg.etcOverlay) {
      # Also needed with the overlay: switch-to-configuration compares the units in
      # $toplevel/etc with the running ones, and the overlay's activation script compares it to
      # /run/current-system/etc to decide whether to remount.
      system.build.etc = mkForce (
        etc
        // {
          # Needed by the systemd tmpfiles module
          passthru.targets = map (x: x.target) etcEntries;
        }
      );
    })

    (mkIf cfg.etcOverlay {
      system.build.etcMetadataImage = mkForce etc."metadata.erofs";
    })

    (mkIf ((cfg.etc || cfg.etcOverlay || cfg.fewerDerivations) && !overlay) {
      # Upstream's activation refers to its own etc derivation instead of system.build.etc, which
      # would build both. Same as upstream otherwise (the tests compare the toplevels).
      system.build.etcActivationCommands = mkForce ''
        # Set up the statically computed bits of /etc.
        echo "setting up /etc..."
        ${pkgs.perl.withPackages (p: [ p.FileSlurp ])}/bin/perl ${
          builtins.path {
            path = modulesPath + "/system/etc/setup-etc.pl";
            name = "setup-etc.pl";
          }
        } ${if cfg.fewerDerivations then atActivation.etc else "${config.system.build.etc}/etc"}
      '';
    })

    (mkIf cfg.fewerDerivations {
      system.systemBuilderCommands = lib.optionalString overlay ''
        ln -s ${config.system.build.etcMetadataImage} $out/etc-metadata-image
        ln -s ${config.system.build.etcBasedir} $out/etc-basedir
      '';
    })

    (mkIf (cfg.fewerDerivations && overlay) {
      system.build.etcActivationCommands = mkForce overlayActivationCommands;

      # nixos-init's bootspec extension refers to /etc. Add those paths in the toplevel builder,
      # so that boot.json's derivation doesn't depend on /etc.
      boot.bootspec.extensions."org.nixos.nixos-init.v1" = {
        etc_metadata_image = mkForce null;
        etc_basedir = mkForce null;
      };
      boot.bootspec.writer = mkForce ''
        ${options.boot.bootspec.writer.default}
        ${lib.getExe pkgs.buildPackages.jq} --sort-keys \
          --arg image ${config.system.build.etcMetadataImage} --arg basedir ${config.system.build.etcBasedir} \
          '."org.nixos.nixos-init.v1" += { etc_metadata_image: $image, etc_basedir: $basedir }' \
          $out/${config.boot.bootspec.filename} > bootspec.tmp
        mv bootspec.tmp $out/${config.boot.bootspec.filename}
      '';
    })

    (mkIf cfg.systemdUnits {
      _module.args.utils = mkForce (
        lib.recursiveUpdate upstreamUtils { systemdUtils.lib = { inherit generateUnits; }; }
      );
    })

    (mkIf cfg.buildEnv {
      system.path = mkForce (
        upstream.system.path.overrideAttrs {
          buildCommand = ''
            ${exe} build-env
            eval "$postBuild"
          '';
        }
      );
    })

    (mkIf cfg.fixDependencies {
      # dbus reads its service directories from system.path at build time, which makes /etc (and
      # every unit referring to the dbus config) depend on it. Resolve it at run time instead.
      services.dbus.packages = mkForce (
        map (p: if isSystemPath p then "/run/current-system/sw" else p) upstream.services.dbus.packages
      );
      environment.etc.terminfo.source = mkForce "/run/current-system/sw/share/terminfo";
    })

    (mkIf (cfg.switchToConfiguration && config.system.switch.enable) {
      # Upstream's wrapper, which sets the environment, with the program it wraps replaced.
      # Replacing the program in upstream's builder commands would need them from the system
      # without our options, whose activation script is another one with fewerDerivations.
      system.activatableSystemBuilderCommands = lib.mkAfter ''
        wrapped=$out/bin/.switch-to-configuration-wrapped
        if [[ $(readlink "$wrapped") != ${upstreamSwitchToConfiguration} ]]; then
          echo "nixosBuildHelpers.switchToConfiguration: switch-to-configuration isn't wrapped as before. nixos-build-helpers needs an update." >&2
          exit 1
        fi
        ln -sfn ${lib.getExe switchToConfiguration} "$wrapped"
      '';
    })

    (mkIf (cfg.systemdBoot && config.boot.loader.systemd-boot.enable) {
      system.build.installBootLoader = mkForce systemdBootInstaller;
      # For the VM test, which compares what both write to the ESP.
      system.build.upstreamInstallBootLoader = upstream.system.build.installBootLoader;
    })

    {
      # Detect when nixpkgs changes something we override. The tests check the outputs.
      assertions = [
        {
          assertion =
            cfg.systemdUnits -> lib.functionArgs upstreamGenerateUnits == lib.functionArgs generateUnits;
          message = "nixosBuildHelpers.systemdUnits: the arguments of nixpkgs' generateUnits changed (${builtins.toJSON (lib.functionArgs upstreamGenerateUnits)}). nixos-build-helpers needs an update.";
        }
        {
          assertion =
            cfg.buildEnv
            -> builtins.match upstreamBuildEnvCommand upstream.system.path.drvAttrs.buildCommand != null;
          message = "nixosBuildHelpers.buildEnv: the build command of nixpkgs' buildEnv changed. nixos-build-helpers needs an update.";
        }
      ];
      warnings =
        lib.optional (cfg.fixDependencies && !lib.any isSystemPath upstream.services.dbus.packages)
          "nixosBuildHelpers.fixDependencies: nixpkgs' services.dbus.packages no longer contains system.path, the dbus override is obsolete.";
    }
  ];
}
