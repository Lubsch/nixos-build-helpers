# systemdBoot: the patched installer writes the same ESP as upstream's, and starts bootctl only
# when needed. The VM boots without a boot loader; the ESP is an extra disk, which bootctl accepts
# as one with SYSTEMD_RELAX_ESP_CHECKS (no partition table), and EFI variables aren't touched.
{
  testers,
  module ? ../module.nix,
}:
testers.runNixOSTest {
  name = "nixos-build-helpers-systemd-boot";
  nodes.machine =
    { config, ... }:
    {
      imports = [ module ];
      nixosBuildHelpers.systemdBoot = true;
      boot.loader.systemd-boot.enable = true;
      boot.loader.efi.canTouchEfiVariables = false;
      virtualisation.emptyDiskImages = [ 64 ];
      environment.etc."nbh-upstream-installer".source = config.system.build.upstreamInstallBootLoader;
      environment.etc."nbh-installer".source = config.system.build.installBootLoader;
    };

  testScript = ''
    machine.wait_for_unit("multi-user.target")
    machine.succeed("mkfs.vfat /dev/vdb && mkdir -p /boot && mount /dev/vdb /boot")
    # Test VMs aren't installed into a profile: two generations of the same system
    machine.succeed("cd /nix/var/nix/profiles && for g in 1 2; do ln -s $(readlink -f /run/current-system) system-$g-link; done && ln -s system-2-link system")
    env = "SYSTEMD_RELAX_ESP_CHECKS=1"
    system = "$(readlink -f /run/current-system)"
    machine.succeed(f"{env} NIXOS_INSTALL_BOOTLOADER=1 /etc/nbh-installer {system}")

    # What's on the ESP, by content (the marker is ours only)
    def esp():
        return machine.succeed("cd /boot && find . -type f ! -name .nixos-bootctl-update | sort | xargs sha256sum")

    def install(installer):
        return machine.succeed(f"{env} {installer} {system} 2>&1")

    install("/etc/nbh-upstream-installer")
    theirs = esp()
    out = install("/etc/nbh-installer")
    assert "same boot loader version" in out, f"bootctl didn't run without the marker:\n{out}"
    assert esp() == theirs, f"{esp()}\nvs. upstream's:\n{theirs}"

    # With the marker for this systemd, bootctl isn't started
    out = install("/etc/nbh-installer")
    assert "boot loader version" not in out, out
    assert esp() == theirs

    # An old generation's entry removed by hand is written again, as upstream does
    machine.succeed("rm /boot/loader/entries/nixos-*.conf")
    install("/etc/nbh-installer")
    assert esp() == theirs

    # /boot not mounted: the mount point check fails, like check-mountpoints
    machine.succeed("umount /boot")
    out = machine.fail(f"{env} /etc/nbh-installer {system} 2>&1")
    assert "is not a mounted partition" in out, out
  '';
}
