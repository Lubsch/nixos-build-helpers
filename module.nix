{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:
let
  cfg = config.systemBuildHelper;
  nixos-build-helpers = pkgs.callPackage ./package.nix { };
in
{
  options.systemBuildHelper = {
    etc = lib.mkEnableOption "NixOS build helper for etc overlayfs";
    etcOverlay = lib.mkEnableOption "NixOS build helper for etc";
    systemdUnits = lib.mkEnableOption "NixOS build helper for systemd units";
  };

  config = {
    system.build =
      let
        etc' = lib.filter (f: f.enable) (lib.attrValues config.environment.etc);
        etc-json = pkgs.writeText "etc-json" (builtins.toJSON etc');
      in
      {
        etcMetadataImage = lib.mkIf cfg.etcOverlay (
          lib.mkForce (
            pkgs.runCommandLocal "etc-metadata.erofs"
              {
                nativeBuildInputs = with pkgs.buildPackages; [
                  composefs
                  erofs-utils
                ];
              }
              ''
                ${lib.getExe nixos-build-helpers} build-composefs-dump ${etc-json} > ./etc-dump
                mkcomposefs --from-file ./etc-dump $out
                fsck.erofs $out
              ''
          )
        );
        etc = lib.mkIf cfg.etc (
          lib.mkForce (
            pkgs.runCommandLocal "etc" {
              # This is needed for the systemd module
              passthru.targets = map (x: x.target) etc';
            } "${lib.getExe nixos-build-helpers} build-etc ${etc-json}"
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
              let
                args-json = pkgs.writeText "generate-${type}-units-args.json" (
                  builtins.toJSON {
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
                  }
                );
              in
              pkgs.runCommand "${type}-units" {
                preferLocalBuild = true;
                allowSubstitutes = false;
              } "${lib.getExe nixos-build-helpers} generate-units ${args-json}";
          }
        )
      );
  };
}
