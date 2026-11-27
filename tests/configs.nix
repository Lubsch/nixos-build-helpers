# NixOS configurations the outputs are compared on. `overlay` selects system.etc.overlay.
{ fixtures }:
let
  base =
    { lib, overlay, ... }:
    {
      fileSystems."/".fsType = "tmpfs";
      boot.loader.grub.devices = [ "/dev/sda" ];
      system.stateVersion = "26.05";
      system.etc.overlay.enable = overlay;
      services.userborn.enable = lib.mkIf overlay true;
    };
in
{
  minimal = base;

  # Exercise the edge cases of every builder.
  featureful =
    {
      pkgs,
      lib,
      overlay,
      ...
    }:
    {
      imports = [ base ];

      environment.etc = {
        "nbh/text".text = "hello";
        "nbh/dash".text = "-"; # a bare '-' needs escaping in a composefs dump
        "nbh/escapes".text = "a b=c\\d\te\rf\nä\n";
        "nbh/large" = {
          text = lib.concatStrings (lib.genList (i: "line ${toString i}\n") 800); # > 4096 bytes
          mode = "0640";
        };
        "nbh/inline-max".text = lib.fixedWidthString 4096 "x" ""; # exactly 4096
        "nbh/empty" = {
          text = "";
          mode = "0600";
        };
        "nbh/owned" = {
          text = "owned";
          mode = "0400";
          user = "nobody";
          group = "nogroup";
        };
        "nbh/numeric-owner" = {
          text = "numeric";
          mode = "440";
          uid = 1234;
          gid = 5678;
        };
        "nbh/glob".source = "${fixtures.dotfiles}/d/*";
        "nbh/directory".source = "${fixtures.dotfiles}/d";
        # nixpkgs' etc-lowerdir can't copy directories (cp without -r)
        "nbh/directory-copy" = lib.mkIf (!overlay) {
          source = "${fixtures.dotfiles}/d";
          mode = "0755";
        };
        "nbh/direct" = {
          source = "${fixtures.a}/share/x/file1";
          mode = "direct-symlink";
        };
        "odd" = {
          target = "nbh//odd/./path";
          text = "odd";
        };
        "dup-1" = {
          target = "nbh/dup";
          source = "${fixtures.a}/share/x/file1";
        };
        "dup-2" = {
          target = "nbh/dup";
          source = "${fixtures.a}/share/x/file1";
        };
        "nbh/disabled" = {
          text = "disabled";
          enable = false;
        };
        "nbh/local-path".source = ./fixtures.nix;
      };

      systemd = {
        packages = [
          fixtures.unitsA
          fixtures.unitsB
        ];
        services = {
          nbh-plain = {
            script = "true";
            wantedBy = [ "multi-user.target" ];
            requiredBy = [ "nbh-etc.target" ];
            upheldBy = [ "nbh-pkg.target" ];
            aliases = [ "nbh-alias.service" ];
          };
          # Collides with a unit from systemd.packages -> drop-in
          nbh-pkg.serviceConfig.Environment = "X=1";
          # Collides with an upstream unit -> drop-in
          systemd-journald.serviceConfig.Environment = "X=1";
          nbh-dropin = {
            overrideStrategy = "asDropin";
            serviceConfig.Environment = "X=1";
          };
          # Masked upstream unit
          systemd-udev-settle.enable = false;
        };
        user.services.nbh-user = {
          script = "true";
          wantedBy = [ "default.target" ];
        };
        nspawn.nbh.execConfig.Boot = true;
      };
      boot.initrd.systemd = {
        services.nbh-initrd = {
          script = "true";
          wantedBy = [ "initrd.target" ];
        };
      };

      environment.systemPackages = fixtures.buildEnvPaths ++ [ pkgs.gsettings-desktop-schemas ];
      environment.pathsToLink = [ "/share/x" ];
      environment.extraOutputsToInstall = [ "man" ];
      environment.extraSetup = "echo extra > $out/share/nbh-extra-setup";

      services.dbus.packages = [ fixtures.dbusOnly ];
    };
}
