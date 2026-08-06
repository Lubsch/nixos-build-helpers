{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:
let
  cfg = config.nixosBuildHelpers;
  nixos-build-helpers = pkgs.callPackage ./package.nix { };
in
{
  options.nixosBuildHelpers = {
    etc = lib.mkEnableOption "Faster builder for etc";
    etcOverlay = lib.mkEnableOption "Faster builder for etc when using overlayfs";
    systemdUnits = lib.mkEnableOption "Faster builder for systemd units (e.g. symstem-units, user-units, ...)";
    buildEnv = lib.mkEnableOption "Faster builder for system-path";
    fixDependencies = lib.mkEnableOption "Make etc not depend on system-path";
  };

  config = {
    system.extraDependencies = [
      # avoid garbage collection which would make offline rebuilds impossible
      nixos-build-helpers
      # tripwire test to detect references to system.path or system.build.etc
      (pkgs.runCommand "tripwire-check" {} ''${./tripwire.sh} ${pkgs.path} && touch $out'')
    ];

    # Dbus usually depends on system.path
    # Use this hack instead
    services.dbus.packages = lib.mkIf (cfg.fixDependencies) (
      lib.mkForce [
        config.services.dbus.dbusPackage
        { outPath = "/run/current-system/sw"; }
      ]
    );

    # Don't depend on terminfo in system.path
    # Just rely on terminfo which has many terminals' terminfo
    environment.etc.terminfo = lib.mkIf (cfg.fixDependencies) (
      lib.mkForce {
        source = "${pkgs.ncurses}/share/terminfo";
      }
    );

    system.path = lib.mkIf cfg.buildEnv (
      lib.mkForce (
        (pkgs.buildEnv {
          name = "system-path";
          paths = config.environment.systemPackages;
          inherit (config.environment) pathsToLink extraOutputsToInstall;
          ignoreCollisions = true;
          # !!! Hacky, should modularise.
          # outputs TODO: note that the tools will often not be linked by default
          postBuild = ''
            # Remove wrapped binaries, they shouldn't be accessible via PATH.
            find $out/bin -maxdepth 1 -name ".*-wrapped" -type l -delete
            find $out/bin -maxdepth 1 -name ".*-wrapped_*" -type l -delete
            if [ -x $out/bin/glib-compile-schemas -a -w $out/share/glib-2.0/schemas ]; then
                $out/bin/glib-compile-schemas $out/share/glib-2.0/schemas
            fi

            ${config.environment.extraSetup}
          '';
          # This override is all we've changed...
        }).overrideDerivation
          (_: {
            buildCommand = ''
              ${lib.getExe nixos-build-helpers} build-env
              eval "$postBuild"
            '';
          })
      )
    );

    system.build =
      let
        etc' = lib.filter (f: f.enable) (lib.attrValues config.environment.etc);
      in
      {
        etcMetadataImage = lib.mkIf cfg.etcOverlay (
          lib.mkForce (
            pkgs.runCommandLocal "etc-metadata.erofs"
              {
                __structuredAttrs = true;
                inherit etc';
                nativeBuildInputs = with pkgs.buildPackages; [
                  composefs
                  erofs-utils
                ];
              }
              ''
                ${lib.getExe nixos-build-helpers} build-composefs-dump > ./etc-dump
                mkcomposefs --from-file ./etc-dump $out
                fsck.erofs $out
              ''
          )
        );

        # Change how system.build.etc is built or avoid building it at all
        etc = lib.mkIf (cfg.etc || cfg.etcOverlay) (
          lib.mkForce (
            (
              if cfg.etc then
                pkgs.runCommandLocal "etc" {
                  __structuredAttrs = true;
                  inherit etc';
                } "${lib.getExe nixos-build-helpers} build-etc"
              else
                # Only here to be linked in by toplevel
                # Usually the etc derivation contains a subdir called etc
                # This hack links it to your run-time / which containts /etc
                # which is your run-time overlayfs
                { outPath = "/"; }
            )
            // {
              # This is needed for the systemd module
              passthru.targets = map (x: x.target) etc';
            }
          )
        );
      };

    # Very hacky override. It might be better to "just" patch nixpkgs
    _module.args.utils =
      let
        utilsBase = import "${modulesPath}/../lib/utils.nix" { inherit lib config pkgs; };
      in
      lib.mkIf cfg.systemdUnits (
        lib.mkForce (
          lib.recursiveUpdate utilsBase {
            systemdUtils.lib.generateUnits =
              let
                cfg = config.systemd;
              in
              {
                allowCollisions ? true,
                type,
                units,
                upstreamUnits,
                upstreamWants,
                packages ? cfg.packages,
                package ? cfg.package,
                defaultUnit ? cfg.defaultUnit,
                ctrlAltDelUnit ? cfg.ctrlAltDelUnit,
              }:
              pkgs.runCommand "${type}-units" {
                __structuredAttrs = true;
                preferLocalBuild = true;
                allowSubstitutes = false;
                generate-units-args = {
                  inherit
                    allowCollisions
                    type
                    units
                    upstreamUnits
                    upstreamWants
                    packages
                    package
                    defaultUnit
                    ctrlAltDelUnit
                    ;
                };
              } "${lib.getExe nixos-build-helpers} generate-units";
          }
        )
      );

    assertions = [
      {
        assertion = !(cfg.etc && cfg.etcOverlay);
        message = "etcOverlay disables building etc completely";
      }
    ];

  };
}
