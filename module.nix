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
    etc = lib.mkEnableOption "NixOS build helper for etc overlayfs";
    etcOverlay = lib.mkEnableOption "NixOS build helper for etc";
    systemdUnits = lib.mkEnableOption "NixOS build helper for systemd units";
  };

  config = {
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
        etc = lib.mkIf (cfg.etc || cfg.etcOverlay) (
          lib.mkForce (
            if cfg.etc then
              pkgs.runCommandLocal "etc" {
                __structuredAttrs = true;
                inherit etc';
                # This is needed for the systemd module
                passthru.targets = map (x: x.target) etc';
              } "${lib.getExe nixos-build-helpers} build-etc"
            else
              {
                outPath = "/";
                passthru.targets = map (x: x.target) etc';
              }
            )
        );
      };

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

  };
}
