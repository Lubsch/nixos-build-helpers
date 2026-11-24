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
