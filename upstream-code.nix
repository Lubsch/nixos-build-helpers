# Code in nixpkgs that nixos-build-helpers reimplements or relies on, by the options that do.
#
# Checked twice, so that a nixpkgs change fails loudly wherever it shows up:
# - by the module, at evaluation time, for the enabled options, against the nixpkgs the system is
#   built with (lib/upstream-check.nix, in Nix)
# - by tests/upstream.nix against this flake's nixpkgs (with sed and grep, which also checks the
#   module's implementation)
#
# On a change: review it, port it (with a test) if needed, then update the hash.
{
  # Whole files (`range = null`) or the lines from one matching `range.from` to the next one
  # matching `range.to`, both included (sed -E -n '/^from$/,/^to$/p'). Patterns are POSIX extended
  # regular expressions matching a whole line.
  reimplemented = [
    {
      options = [ "buildEnv" ];
      file = "pkgs/build-support/buildenv/builder.pl";
      range = null;
      sha256 = "71e762e76f4efc9178dc2048e3e418811f3cb521e9e80321eb36f9091623eb82";
    }
    {
      options = [ "etcOverlay" ];
      file = "nixos/modules/system/etc/build-composefs-dump.py";
      range = null;
      sha256 = "440c68d7363f2e9be32d3860ef1c48049722108ac8cffb97662944d06cb50c71";
    }
    {
      options = [ "systemdUnits" ];
      file = "nixos/lib/systemd-lib.nix";
      range = {
        from = "  generateUnits =.*";
        to = "  makeJobScript =.*";
      };
      sha256 = "04ab63fd88682f36f4c81b78dbe7c0d98ddcc50ae53cba3454e60230d66af74f";
    }
    {
      options = [
        "etc"
        "etcOverlay"
      ];
      file = "nixos/modules/system/etc/etc.nix";
      range = {
        from = "  etc =";
        to = "  etcHardlinks =.*";
      };
      sha256 = "482d461bba6a0a21e513228fefff11ebc6afcd014bcf6842fa253842600e78f1";
    }
    {
      # The install-info loop that buildEnv runs in one process (src/install_info.rs)
      options = [ "buildEnv" ];
      file = "nixos/modules/misc/documentation.nix";
      range = {
        from = "      environment.extraSetup = ''";
        to = "      '';";
      };
      sha256 = "74c53ba43fc937f5a905a063f73111b27e6220dd66812aef747b193f1218a7af";
    }
    {
      # The update-mime-database that buildEnv runs ported (src/update_mime_database.rs)
      options = [ "buildEnv" ];
      file = "nixos/modules/config/xdg/mime.nix";
      range = {
        from = "    environment.extraSetup = ''";
        to = "    '';";
      };
      sha256 = "fff446e4860c9d9107dd31c2e1b04ea3f3945470aa89f62859b6910e2bcfc1fe";
    }
  ];

  # buildEnv, when system.path runs update-mime-database (xdg.mime):
  # src/update_mime_database.rs is a port of update-mime-database.cpp of these
  # versions (2.4 and 2.5.1 differ only in their version), unpatched, and of what it uses of
  # glib's hash table (src/ghash.rs) and libxml2 (src/mime_xml.rs), in the versions it was
  # compared with.
  sharedMimeInfo = {
    versions = [
      "2.4"
      "2.5.1"
    ];
    patches = [ ];
    glib = [
      "2.88.1"
      "2.90.0"
    ];
    libxml2 = [
      "2.15.3"
      "2.15.4"
    ];
  };

  # buildEnv, when system.path runs install-info (documentation.info): src/install_info.rs is a
  # port of install-info.c of these texinfo versions, unpatched
  texinfo = {
    versions = [ "7.2" ];
    patches = [ ];
  };

  # etcOverlay: must match INLINE_CONTENT_MAX in src/build_composefs_dump.rs
  etcInlineContentMax = {
    file = "nixos/modules/system/etc/etc.nix";
    text = "etcInlineContentMax = 4096;";
  };

  # fixDependencies: the lines mentioning system.path (grep -H 'system\.path\b', sorted bytewise),
  # in these files. A use that reads system.path at build time would break with
  # /run/current-system/sw, one in e.g. restartTriggers changes behaviour (see the README).
  # tests/upstream.nix also searches all of nixos/modules for new files.
  systemPathUses = {
    files = [
      "nixos/modules/config/system-path.nix"
      "nixos/modules/config/terminfo.nix"
      "nixos/modules/config/users-groups.nix"
      "nixos/modules/security/polkit.nix"
      "nixos/modules/services/desktop-managers/gnome.nix"
      "nixos/modules/services/desktops/accountsservice.nix"
      "nixos/modules/services/scheduling/cron.nix"
      "nixos/modules/services/scheduling/fcron.nix"
      "nixos/modules/services/security/paretosecurity.nix"
      "nixos/modules/services/system/dbus.nix"
      "nixos/modules/services/x11/desktop-managers/enlightenment.nix"
      "nixos/modules/services/x11/desktop-managers/lxqt.nix"
      "nixos/modules/system/activation/top-level.nix"
    ];
    sha256 = "c9f971379eb21bf89f1ddcd038c97ac2096e71f3a51c647dd48a11a43b7096ae";
  };

  # buildEnv builds system.path without stdenv's setup (~35 ms), with what of it a buildCommand
  # without inputs sees (`withoutStdenv` in module.nix). tests/upstream.nix compares this with what
  # stdenv's setup provides.
  stdenv = {
    # Exported by stdenv's setup, besides what nix sets. `withoutStdenv` sets all but the marked.
    exports = [
      "CONFIG_SHELL"
      "GZIP_NO_TIMESTAMPS"
      "HOST_PATH" # not set: for patchShebangs in fixupPhase
      "NIX_CFLAGS_COMPILE" # not set: compiler wrapper
      "NIX_ENFORCE_NO_NATIVE" # not set: compiler wrapper
      "NIX_ENFORCE_PURITY" # not set: compiler wrapper
      "NIX_LDFLAGS" # not set: compiler wrapper
      "NIX_SSL_CERT_FILE"
      "PATH" # without the default native build inputs (patchelf, for fixupPhase)
      "SHELL"
      "SOURCE_DATE_EPOCH"
      "SSL_CERT_FILE"
      "TZ"
      "XDG_DATA_DIRS" # not set: patchelf's share
      "out"
      "shell"
    ];
    # The options a buildCommand runs with (`$-`)
    flags = "ehB";
    # Its shell functions: system.path is built with stdenv when its postBuild mentions one.
    functions = [
      "_activatePkgs"
      "_addRpathPrefix"
      "_addToEnv"
      "_allFlags"
      "_assignFirst"
      "_callImplicitHook"
      "_defaultUnpack"
      "_doStrip"
      "_eval"
      "_exportOutputsByName"
      "_logHook"
      "_makeSymlinksRelative"
      "_moveLib64"
      "_moveSbin"
      "_moveSystemdUserUnits"
      "_moveToShare"
      "_multioutConfig"
      "_multioutDevs"
      "_multioutDocs"
      "_multioutPropagateDev"
      "_nixLogWithLevel"
      "_overrideFirst"
      "_pruneLibtoolFiles"
      "_updateSourceDateEpochFromSourceRoot"
      "activatePackage"
      "addEnvHooks"
      "addToSearchPath"
      "addToSearchPathWithCustomDelimiter"
      "appendToVar"
      "auditTmpdir"
      "buildPhase"
      "checkPhase"
      "compressManPages"
      "concatStringsSep"
      "concatTo"
      "configurePhase"
      "consumeEntire"
      "definePhases"
      "distPhase"
      "dumpVars"
      "echoCmd"
      "exitHandler"
      "findInputs"
      "fixLibtool"
      "fixupPhase"
      "genericBuild"
      "getAllOutputNames"
      "installCheckPhase"
      "installPhase"
      "isELF"
      "isMachO"
      "isScript"
      "mapOffset"
      "moveToOutput"
      "nixChattyLog"
      "nixDebugLog"
      "nixErrorLog"
      "nixInfoLog"
      "nixLog"
      "nixNoticeLog"
      "nixTalkativeLog"
      "nixVomitLog"
      "nixWarnLog"
      "noBrokenSymlinks"
      "noBrokenSymlinksInAllOutputs"
      "patchELF"
      "patchPhase"
      "patchShebangs"
      "patchShebangsAuto"
      "prependToVar"
      "printLines"
      "printPhases"
      "printWords"
      "recordPropagatedDependencies"
      "runHook"
      "runOneHook"
      "runPhase"
      "showPhaseFooter"
      "showPhaseHeader"
      "stripDirs"
      "stripHash"
      "substitute"
      "substituteAll"
      "substituteAllInPlace"
      "substituteAllStream"
      "substituteInPlace"
      "substituteStream"
      "unpackFile"
      "unpackPhase"
      "updateAutotoolsGnuConfigScriptsPhase"
      "updateSourceDateEpoch"
    ];
  };

  # fewerDerivations with the /etc overlay: the fields of nixos-init's bootspec extension. Two refer
  # to /etc, which the toplevel's builder adds instead; a new one might refer to it too.
  nixosInitBootspecFields = [
    "env_binary"
    "etc_basedir"
    "etc_metadata_image"
    "firmware"
    "modprobe_binary"
    "nix_store_mount_opts"
    "sh_binary"
  ];
}
