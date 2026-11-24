{
  config,
  lib,
  pkgs,
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
  upstream =
    (extendModules {
      modules = [
        # Stronger than mkForce, so that this also works on variants that mkForce our options.
        (lib.mkOverride 1 {
          nixosBuildHelpers = lib.mapAttrs (_: _: false) cfg;
          nixpkgs.pkgs = pkgs;
        })
      ];
    }).config;

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
      inherit
        allowCollisions
        type
        upstreamUnits
        upstreamWants
        packages
        package
        ;
      inherit (config.systemd) defaultUnit ctrlAltDelUnit;
      units = lib.mapAttrs (_: u: {
        unit = "${u.unit}";
        overrideStrategy = u.overrideStrategy or "asDropinIfExists";
        aliases = u.aliases or [ ];
        wantedBy = u.wantedBy or [ ];
        upheldBy = u.upheldBy or [ ];
        requiredBy = u.requiredBy or [ ];
      }) units;
    } "${exe} generate-units";

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

    (mkIf ((cfg.etc || cfg.etcOverlay) && !config.system.etc.overlay.enable) {
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
        } ${config.system.build.etc}/etc
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
