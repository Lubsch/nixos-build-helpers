# The derivations our builders replace, wrapped to run their build command `runs` times inside the
# build sandbox and record the wall time of each run (in ns) as $out. Measuring inside the sandbox
# excludes sandbox setup and scheduling, which are the same for every builder.
{
  runs,
  salt ? "",
  ...
}@args:
let
  system = import ./system.nix (
    removeAttrs args [
      "runs"
      "salt"
    ]
  );
  inherit (system) config pkgs;
  inherit (pkgs) lib;
  overlay = config.system.etc.overlay.enable;

  # Run a derivation's build `runs` times, recording the times.
  timed =
    drv:
    if drv ? overrideAttrs || drv ? overrideDerivation then
      # The previous version's system.path only has overrideDerivation
      (drv.overrideAttrs or drv.overrideDerivation) (o: {
        inherit salt;
        origBuildCommand = o.buildCommand;
        # Only time what we replace: system.path's postBuild (glib-compile-schemas, extraSetup) is
        # the same for all variants.
        postBuild = "";
        buildCommand = ''
          times=()
          for i in $(seq ${toString runs}); do
            rm -rf "$out"
            start=$(date +%s%N)
            ( eval "$origBuildCommand" ) > /dev/null 2>&1
            times+=($(( $(date +%s%N) - start )))
          done
          rm -rf "$out"
          printf '%s\n' "''${times[@]}" > "$out"
        '';
      })
    else
      # A plain derivation with a `bash -euc SCRIPT` builder (our etc with etcOverlay)
      derivation (
        drv.drvAttrs
        // {
          inherit salt;
          origScript = lib.elemAt drv.drvAttrs.args 1;
          args = [
            "-euc"
            ''
              source "$NIX_ATTRS_SH_FILE"
              times=()
              for ((i = 0; i < ${toString runs}; i++)); do
                ${pkgs.coreutils}/bin/rm -rf "''${outputs[@]}"
                start=$EPOCHREALTIME
                ( ${pkgs.bash}/bin/bash -euc "$origScript" ) > /dev/null 2>&1
                end=$EPOCHREALTIME
                times+=($(( ''${end/./} - ''${start/./} ))000)
              done
              ${pkgs.coreutils}/bin/rm -rf "''${outputs[@]}"
              for o in "''${outputs[@]}"; do : > "$o"; done
              printf '%s\n' "''${times[@]}" > "''${outputs[out]}"
            ''
          ];
        }
      );

  # Upstream builds the composefs dump in a separate derivation (mirrors etc.nix).
  upstreamEtcDump = pkgs.runCommandLocal "etc-dump" { } ''
    ${lib.getExe pkgs.buildPackages.python3} ${pkgs.path}/nixos/modules/system/etc/build-composefs-dump.py ${
      pkgs.writeText "etc-json" (
        builtins.toJSON (lib.filter (f: f.enable) (lib.attrValues config.environment.etc))
      )
    } > $out
  '';
  etc = config.environment.etc;
  replaced = config.nixosBuildHelpers.etcOverlay;
in
lib.mapAttrs (_: map timed) (
  lib.filterAttrs (_: v: v != [ ]) {
    system-path = [ config.system.path ];
    system-units = [ etc."systemd/system".source ];
    user-units = [ etc."systemd/user".source ];
    initrd-units =
      lib.optional config.boot.initrd.systemd.enable
        config.boot.initrd.systemd.contents."/etc/systemd/system".source;
    etc = lib.optional (!overlay) config.system.build.etc;
    # Everything built for /etc with the overlay, including mkcomposefs and fsck.erofs, which all
    # variants run. (Upstream: etc, the dump and the image. The previous version: only the image,
    # with system.build.etc = "/". Current: etc and the image in one derivation.)
    etc-overlay = lib.optionals overlay (
      # once per derivation, not per output
      lib.attrValues (
        lib.listToAttrs (
          map (d: lib.nameValuePair (builtins.unsafeDiscardStringContext d.drvPath) d) (
            lib.optional (lib.isDerivation config.system.build.etc) config.system.build.etc
            ++ lib.optional (!replaced) upstreamEtcDump
            ++ [ config.system.build.etcMetadataImage ]
          )
        )
      )
    );
  }
)
