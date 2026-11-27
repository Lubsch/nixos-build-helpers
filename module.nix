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

  # system.path as it's built: upstream's, with buildEnv by our builder.
  systemPath =
    if cfg.buildEnv then
      upstream.system.path.overrideAttrs {
        buildCommand = ''
          ${exe} build-env
          eval "$postBuild"
        '';
      }
    else
      upstream.system.path;

  # With fixDependencies, what modules get as config.system.path: the same attributes, but
  # interpolated it's /run/current-system/sw, without string context. So whatever refers to it
  # (dbus' service directories, /etc/terminfo, crontabs, unit environments, restartTriggers, …)
  # resolves it at run time, and doesn't depend on it at build time. The toplevel links the real one.
  systemPathAtRunTime = systemPath // {
    outPath = "/run/current-system/sw";
  };

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

  # suid-sgid-wrappers: upstream's script, skipped when the wrappers it would create are in place.
  # sysinit-reactivation.target restarts the unit on every switch, and the script starts ~5
  # processes per wrapper (cp, chmod, chown, chmod; setcap), ~65-110 ms for 11 wrappers, though
  # the script rarely changes (it was the same in 24 generations in a row on one machine).
  # After a run, a stamp records the hash of the script and the directory it made. A later run
  # with the same script skips when /run/wrappers/bin is still that directory and every wrapper is
  # in it. Bash builtins only, so a skip starts no process. The upstream script is unchanged, so a
  # changed or new wrapper, a reboot (/run is a tmpfs) or a run without this option (a new
  # directory) makes it run again. Not restored by a skip: a wrapper whose mode, owner or contents
  # someone changed by hand (deleting one is noticed).
  upstreamWrappersScript = upstream.systemd.services.suid-sgid-wrappers.script;
  wrappersDir = config.security.wrapperDir;
  wrappersStamp = "${dirOf wrappersDir}/.nixos-build-helpers-stamp";
  wrapperPrograms = map (w: w.program) (
    lib.filter (w: w.enable) (lib.attrValues config.security.wrappers)
  );
  wrappersScriptHash = builtins.hashString "sha256" (
    builtins.unsafeDiscardStringContext upstreamWrappersScript
  );
  wrappersPresent = lib.concatMapStrings (
    p: " && -e ${wrappersDir}/${lib.escapeShellArg p}"
  ) wrapperPrograms;
  wrappersScript = ''
    # nixosBuildHelpers.wrappers: skip when this script already made the wrappers in place.
    if [[ -e ${wrappersStamp} ]]; then
      { read -r hash; read -r dir; } < ${wrappersStamp} || true
      if [[ $hash == ${wrappersScriptHash} && -n $dir && ${wrappersDir} -ef $dir${wrappersPresent} ]]; then
        exit 0
      fi
    fi
    rm -f ${wrappersStamp}

    ${upstreamWrappersScript}

    # Only reached when everything above succeeded (unit scripts run with `set -e`).
    printf '%s\n' ${wrappersScriptHash} "$wrapperDir" > ${wrappersStamp}
  '';

  # Upstream code that the enabled options reimplement or rely on, checked against this system's
  # nixpkgs (see upstream-code.nix).
  upstreamCode = import ./upstream-code.nix;
  upstreamChanges = import ./lib/upstream-check.nix {
    inherit lib;
    pkgsRoot = pkgs.path;
    nixosRoot = "${modulesPath}/../..";
    enabled = cfg;
  };
  needsUpdate = "nixos-build-helpers needs an update (or set nixosBuildHelpers.allowUpstreamChanges to only warn).";

  # fewerDerivations with the overlay: the fields of nixos-init's bootspec extension upstream.
  nixosInitBootspecFields = lib.attrNames (
    upstream.boot.bootspec.extensions."org.nixos.nixos-init.v1" or { }
  );

  # The shape of upstream's build command for buildEnv, which we replace.
  upstreamBuildEnvCommand = builtins.unsafeDiscardStringContext "${pkgs.buildPackages.perl}/bin/perl -w ${builtins.storeDir}/[a-z0-9]{32}-builder.pl\neval \"\\$postBuild\"\n";
in
{
  options.nixosBuildHelpers = {
    etc = mkEnableOption "the faster builder for `system.build.etc`";
    etcOverlay = mkEnableOption "the faster builder for `system.build.etcMetadataImage` (used with `system.etc.overlay`). Implies `etc`";
    systemdUnits = mkEnableOption "the faster builder for systemd units (system-units, user-units, initrd-units, nspawn-units)";
    buildEnv = mkEnableOption "the faster builder for `system.path`";
    fixDependencies = mkEnableOption "removing the dependencies on `system.path` (of /etc, units, …): `config.system.path` refers to /run/current-system/sw, so they build in parallel and aren't rebuilt when only packages change. Services that were restarted on every package change because they referred to it (polkit's restartTriggers, accounts-daemon's XDG_DATA_DIRS) aren't anymore (see the README)";
    switchToConfiguration = mkEnableOption "a patched `switch-to-configuration` that waits less: 10 ms instead of 250 ms for systemd's events to settle, and the user managers are only reexecuted when their systemd or user.conf changes, and only reloaded when something on their unit search path, their generators or what those read changed with the toplevel, and the unit files of the active units are looked up in one batch of D-Bus calls instead of one call after the other. Unlike the other options, this changes behaviour (see the README)";
    systemdBoot = mkEnableOption "a patched systemd-boot installer that starts fewer processes (no nix-env, check-mountpoints or an empty copy-extra-files script; bootctl only when the systemd package changes) and runs byte-compiled. Unlike the other options, this changes behaviour (see the README)";
    wrappers = mkEnableOption "skipping upstream's suid-sgid-wrappers script when the wrappers it would create are in place (every switch restarts it: ~65-110 ms). Unlike most options, this changes behaviour: a switch no longer restores a wrapper changed by hand (see the README)";
    allowUpstreamChanges = mkEnableOption "building when nixpkgs changed code that the enabled options reimplement or rely on (see upstream-code.nix), with a warning instead of an error. Their output may then differ from nixpkgs'";
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
    (mkIf (lib.any lib.id (lib.attrValues (removeAttrs cfg [ "allowUpstreamChanges" ]))) {
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

    (mkIf (cfg.buildEnv && !cfg.fixDependencies) {
      system.path = mkForce systemPath;
    })

    (mkIf cfg.fixDependencies {
      system.path = mkForce systemPathAtRunTime;
      # The real one, e.g. for `nix build .#nixosConfigurations.<name>.config.system.build.systemPath`
      system.build.systemPath = systemPath;
      # Upstream's builder linked config.system.path, i.e. /run/current-system/sw, before this.
      system.systemBuilderCommands = ''
        if [[ $(readlink "$out/sw") != ${systemPathAtRunTime} ]]; then
          echo "nixosBuildHelpers.fixDependencies: the toplevel no longer links config.system.path as sw. nixos-build-helpers needs an update." >&2
          exit 1
        fi
        ln -sfn ${systemPath} $out/sw
      '';
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

    (mkIf (cfg.wrappers && upstream.systemd.services ? suid-sgid-wrappers) {
      systemd.services.suid-sgid-wrappers.script = mkForce wrappersScript;
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
        {
          # The stamp records the directory upstream's script made, by its variable.
          assertion =
            cfg.wrappers
            -> lib.hasInfix "wrapperDir=$(mktemp --directory" upstreamWrappersScript
              && lib.all (p: lib.hasInfix "\"$wrapperDir/${p}\"" upstreamWrappersScript) wrapperPrograms;
          message = "nixosBuildHelpers.wrappers: the shape of nixpkgs' suid-sgid-wrappers script changed. nixos-build-helpers needs an update.";
        }
        {
          # A field added upstream might refer to /etc again.
          assertion =
            cfg.fewerDerivations && overlay
            -> lib.elem "etc_metadata_image" nixosInitBootspecFields
              && lib.all (f: lib.elem f upstreamCode.nixosInitBootspecFields) nixosInitBootspecFields;
          message = "nixosBuildHelpers.fewerDerivations: the fields of nixpkgs' nixos-init bootspec extension changed (${toString nixosInitBootspecFields}). ${needsUpdate}";
        }
      ]
      ++ lib.optionals (!cfg.allowUpstreamChanges) (
        map (m: {
          assertion = false;
          message = "${m} ${needsUpdate}";
        }) upstreamChanges
      );
      warnings = lib.optionals cfg.allowUpstreamChanges (
        map (m: "${m} Its output may differ from nixpkgs'.") upstreamChanges
      );
    }
  ];
}
