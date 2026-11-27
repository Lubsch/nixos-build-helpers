# Boot systems with all options and check that they work at run time, especially what
# fixDependencies resolves at run time instead of build time.
{
  testers,
  overlay,
  module ? ../module.nix,
}:
let
  config =
    { pkgs, ... }:
    {
      system.etc.overlay.enable = overlay;
      services.userborn.enable = overlay;
      # stage 2 without the activation script
      system.nixos-init.enable = overlay;

      # getcap: the wrappers' capabilities are compared with the reference's
      environment.systemPackages = [
        pkgs.foot.terminfo
        pkgs.libcap
      ];
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
      systemd.user.services.nbh-test-user = {
        wantedBy = [ "default.target" ];
        script = "true";
      };
      # A user manager for switch-to-configuration's user scope
      users.users.alice = {
        isNormalUser = true;
        linger = true;
      };
      # A system to switch to, changing a unit and a file in /etc
      specialisation.changed.configuration = {
        systemd.services.nbh-test.environment.CHANGED = "1";
        environment.etc."nbh-test".text = pkgs.lib.mkForce "changed";
      };
      # Changing a user unit
      specialisation.userChanged.configuration = {
        systemd.user.services.nbh-test-user.environment.CHANGED = "1";
      };
      # Another wrapper: the suid-sgid-wrappers script changes
      specialisation.wrapperChanged.configuration = {
        security.wrappers.nbh-test = {
          source = "${pkgs.coreutils}/bin/true";
          owner = "root";
          group = "root";
          setuid = true;
        };
      };
      # The same, plus a package with a user unit (in /run/current-system/sw, not /etc)
      specialisation.packageChanged.configuration = {
        systemd.user.services.nbh-test-user.environment.CHANGED = "1";
        environment.systemPackages = [
          (pkgs.writeTextDir "share/systemd/user/nbh-test-package.service" "[Service]\nExecStart=${pkgs.coreutils}/bin/true\n")
        ];
      };
    };
in
testers.runNixOSTest {
  name = "nixos-build-helpers${if overlay then "-overlay" else ""}";
  nodes.machine = {
    imports = [
      config
      module
    ];
    nixosBuildHelpers = {
      etc = !overlay;
      etcOverlay = overlay;
      systemdUnits = true;
      buildEnv = true;
      fixDependencies = true;
      fewerDerivations = true;
      switchToConfiguration = true;
      wrappers = true;
    };
  };
  # The same system without nixos-build-helpers
  nodes.reference = config;

  testScript = ''
    import re
    start_all()
    machine.wait_for_unit("multi-user.target")
    reference.wait_for_unit("multi-user.target")

    # systemd sees the same unit files, in the same states (e.g. linked, enabled, static)
    for cmd in ["systemctl list-unit-files --no-legend --no-pager",
                "systemctl --global list-unit-files --no-legend --no-pager",
                "systemctl show -p UnitFileState,FragmentPath,DropInPaths,Names nbh-test.service systemd-journald.service"]:
        # (Paths in the store differ, e.g. DropInPaths in system-units without the overlay.)
        ours, theirs = (re.sub("/nix/store/[a-z0-9]{32}-", "/nix/store/<hash>-", m.succeed(cmd)) for m in (machine, reference))
        assert ours == theirs, f"{cmd}:\n{ours}\nvs. without nixos-build-helpers:\n{theirs}"

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

    # Switching to the running system leaves /etc alone (the overlay isn't remounted)
    out = machine.succeed("/run/current-system/bin/switch-to-configuration test 2>&1")
    assert "remounting /etc" not in out, out

    # switch-to-configuration compares the new toplevel's etc with the running units
    machine.wait_for_unit("user@1000.service")
    base = machine.succeed("readlink -f /run/current-system").strip()
    cursor = machine.succeed("journalctl --show-cursor -n0 -q | sed -n 's/^-- cursor: //p'").strip()
    before = machine.succeed("systemctl show -P InvocationID nbh-test.service")
    machine.succeed(f"{base}/specialisation/changed/bin/switch-to-configuration test")
    assert machine.succeed("systemctl show -P InvocationID nbh-test.service") != before, "nbh-test wasn't restarted"
    assert machine.succeed("cat /etc/nbh-test") == "changed"

    # switchToConfiguration: the user manager runs the same systemd and user units: neither
    # reexecuted nor reloaded. (The system manager is reloaded: a system unit changed.)
    def user_manager_log(since):
        return machine.succeed(f"journalctl --after-cursor='{since}' _SYSTEMD_UNIT=user@1000.service -o cat")
    log = user_manager_log(cursor)
    assert "Reexecuting" not in log and "Reloading" not in log, log
    assert "Reloading finished" in machine.succeed(f"journalctl --after-cursor='{cursor}' _PID=1 -o cat")

    # A changed user unit: the user manager is reloaded, still not reexecuted
    cursor = machine.succeed("journalctl --show-cursor -n0 -q | sed -n 's/^-- cursor: //p'").strip()
    machine.succeed(f"{base}/specialisation/userChanged/bin/switch-to-configuration test")
    log = user_manager_log(cursor)
    assert "Reloading finished" in log and "Reexecuting" not in log, log

    # Only a package's user unit is new (the user units in /etc are the same): reloaded too, so
    # that the user manager knows it
    for p in ["etc/systemd/user", "etc/environment.d"]:
        assert machine.succeed(f"readlink -f {base}/specialisation/userChanged/{p}") == machine.succeed(f"readlink -f {base}/specialisation/packageChanged/{p}"), p
    user = "systemctl --machine=alice@ --user"
    def load_state(unit):
        return machine.succeed(f"{user} show -P LoadState {unit}").strip()
    assert load_state("nbh-test-package.service") == "not-found"
    cursor = machine.succeed("journalctl --show-cursor -n0 -q | sed -n 's/^-- cursor: //p'").strip()
    machine.succeed(f"{base}/specialisation/packageChanged/bin/switch-to-configuration test")
    log = user_manager_log(cursor)
    assert "Reloading finished" in log and "Reexecuting" not in log, log
    assert load_state("nbh-test-package.service") == "loaded"

    # wrappers: the same wrappers as nixpkgs makes
    wrappers = "stat -c '%n %a %U:%G %s' /run/wrappers/bin/* && getcap -r /run/wrappers/bin"
    ours, theirs = (m.succeed(wrappers) for m in (machine, reference))
    assert ours == theirs, f"{ours}\nvs. without nixos-build-helpers:\n{theirs}"
    def wrapper_dir():
        return machine.succeed("readlink -f /run/wrappers/bin").strip()
    # The switches above didn't change the script, so they kept the wrappers made at boot,
    # and so does a restart
    made = wrapper_dir()
    machine.succeed("systemctl restart suid-sgid-wrappers.service")
    assert wrapper_dir() == made, "the unchanged script ran again"
    # A deleted wrapper is made again
    machine.succeed("rm /run/wrappers/bin/su")
    machine.succeed("systemctl restart suid-sgid-wrappers.service")
    assert wrapper_dir() != made
    assert machine.succeed(wrappers) == ours
    # Another script (a new wrapper) runs
    made = wrapper_dir()
    machine.succeed(f"{base}/specialisation/wrapperChanged/bin/switch-to-configuration test")
    assert wrapper_dir() != made
    machine.succeed("test -u /run/wrappers/bin/nbh-test")
  '';
}
