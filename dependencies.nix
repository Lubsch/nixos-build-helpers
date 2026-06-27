{ lib, pkgs, ... }: {

  imports = [
    ./dbus-fix.nix
  ];

  # avoid depending on config.system.path
  environment.etc.terminfo = lib.mkForce {
    source = "${pkgs.ncurses}/share/terminfo";
  };

}
