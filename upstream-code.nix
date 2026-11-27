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
  ];

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
