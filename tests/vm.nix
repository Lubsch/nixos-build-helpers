# Boot systems with all options and check that they work at run time, especially what
# fixDependencies resolves at run time instead of build time.
{
  testers,
  overlay,
  module ? ../module.nix,
}:
testers.runNixOSTest {
  name = "nixos-build-helpers${if overlay then "-overlay" else ""}";
  nodes.machine =
    { pkgs, ... }:
    {
      imports = [ module ];
      nixosBuildHelpers = {
        etc = !overlay;
        etcOverlay = overlay;
        systemdUnits = true;
        buildEnv = true;
        fixDependencies = true;
      };
      system.etc.overlay.enable = overlay;
      boot.initrd.systemd.enable = true;
      services.userborn.enable = overlay;
      # stage 2 without the activation script
      system.nixos-init.enable = overlay;

      environment.systemPackages = [ pkgs.foot.terminfo ];
      environment.etc."nbh-test" = {
        text = "-";
        mode = "0640";
      };
      systemd.services.nbh-test = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig.Type = "oneshot";
        serviceConfig.RemainAfterExit = true;
        script = "true";
      };
      # A system to switch to, changing a unit and a file in /etc
      specialisation.changed.configuration = {
        systemd.services.nbh-test.environment.CHANGED = "1";
        environment.etc."nbh-test".text = pkgs.lib.mkForce "changed";
      };
    };

  testScript = ''
    machine.wait_for_unit("multi-user.target")
    machine.succeed("systemctl is-active nbh-test.service")
    assert machine.succeed("systemctl --failed --no-legend") == "", "failed units"
    # /etc
    assert machine.succeed("cat /etc/nbh-test") == "-"
    assert machine.succeed("stat -c %a /etc/nbh-test").strip() == "640"
    # dbus finds its service directories through /run/current-system/sw
    machine.succeed("busctl call org.freedesktop.systemd1 /org/freedesktop/systemd1 org.freedesktop.DBus.Peer Ping")
    machine.succeed("grep -r /run/current-system/sw /etc/dbus-1/")
    # terminfo from systemPackages (foot) is found through /etc/terminfo
    machine.succeed("test -e /etc/terminfo/f/foot")

    # switch-to-configuration compares the new toplevel's etc with the running units
    before = machine.succeed("systemctl show -P InvocationID nbh-test.service")
    machine.succeed("/run/current-system/specialisation/changed/bin/switch-to-configuration test")
    assert machine.succeed("systemctl show -P InvocationID nbh-test.service") != before, "nbh-test wasn't restarted"
    assert machine.succeed("cat /etc/nbh-test") == "changed"
  '';
}
