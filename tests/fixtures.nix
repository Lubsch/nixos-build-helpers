# Small packages exercising edge cases of the builders. Used by the buildEnv tests and the
# "featureful" NixOS test configuration.
{ lib, runCommand }:
let
  mk = name: script: runCommand "nbh-fixture-${name}" { } "mkdir -p $out\ncd $out\n${script}";
in
rec {
  # --- buildEnv ---
  a = mk "a" ''
    mkdir -p bin share/x share/common share/info share/mime/packages share/mime/types lib/perl5
    echo a > bin/foo; chmod +x bin/foo
    echo a > share/x/file1
    echo a > share/common/a.txt
    echo same > share/collide-same
    echo a > share/collide-differ
    echo a > share/info/dir
    echo '<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info"/>' > share/mime/packages/a.xml
    echo a > share/mime/types/a
    echo a > lib/perl5/perllocal.pod
    mkdir -p nix-support; echo a > nix-support/a
    mkdir log; echo a > log/a
  '';
  b = mk "b" ''
    mkdir -p bin share/x share/common
    echo b > bin/foo; chmod +x bin/foo
    echo b > share/x/file2
    echo b > share/common/b.txt
    echo same > share/collide-same
    echo b > share/collide-differ
  '';
  # A file with high priority where another package has a directory.
  hiFile = lib.hiPrio (mk "hi-file" "mkdir share; echo hi > share/dir-vs-file");
  dirPkg = mk "dir" "mkdir -p share/dir-vs-file; echo dir > share/dir-vs-file/inner";
  # A symlink to a file and a directory of another package, at the same path.
  realDir = mk "real-dir" "mkdir -p share/linked/sub; echo real > share/linked/sub/f; echo real > share/linked-file";
  linkDir = mk "link-dir" ''
    mkdir share
    ln -s ${realDir}/share/linked share/linked
    ln -s ${realDir}/share/linked-file share/linked-file
  '';
  dangling = mk "dangling" "mkdir share; ln -s /nonexistent share/dangling";
  propagatedB = mk "propagated-b" "mkdir share; echo pb > share/propagated-b";
  propagatedC = mk "propagated-c" "mkdir share; echo pc > share/propagated-c";
  propagator = mk "propagator" ''
    mkdir -p nix-support share
    echo ${propagatedB} > nix-support/propagated-user-env-packages
    echo ${propagatedC} >> nix-support/propagated-user-env-packages
    echo p > share/propagator
  '';
  lowFile = lib.lowPrio (mk "low-file" "mkdir bin; echo low > bin/foo; chmod +x bin/foo");
  multiOutput =
    runCommand "nbh-fixture-multi"
      {
        outputs = [
          "out"
          "man"
        ];
      }
      ''
        mkdir -p $out/bin $man/share/man/man1
        echo out > $out/bin/multi
        echo man > $man/share/man/man1/multi.1
      '';
  dotfiles = mk "dotfiles" "mkdir -p d share; touch d/a d/b d/.hidden share/.hidden";

  buildEnvPaths = [
    a
    b
    hiFile
    dirPkg
    linkDir
    realDir
    dangling
    propagator
    lowFile
    multiOutput
    dotfiles
  ];

  # --- systemd units ---
  unitsA = mk "units-a" ''
    mkdir -p lib/systemd/system/nbh-pkg.service.d lib/systemd/system/nbh-pkg.target.wants etc/systemd/system lib/systemd/user
    printf '[Service]\nExecStart=/bin/true\n' > lib/systemd/system/nbh-pkg.service
    echo '[Service]' > lib/systemd/system/nbh-pkg.service.d/a.conf
    ln -s a.conf lib/systemd/system/nbh-pkg.service.d/link.conf
    ln -s ../nbh-pkg.service lib/systemd/system/nbh-pkg.target.wants/nbh-pkg.service
    printf '[Unit]\n' > lib/systemd/system/.hidden.service
    printf '[Unit]\n' > etc/systemd/system/nbh-etc.target
    printf '[Service]\nExecStart=/bin/true\n' > lib/systemd/user/nbh-pkg-user.service
  '';
  # Merges into unitsA's drop-in directory.
  unitsB = mk "units-b" ''
    mkdir -p lib/systemd/system/nbh-pkg.service.d
    echo '[Service]' > lib/systemd/system/nbh-pkg.service.d/b.conf
  '';

  # --- dbus ---
  # A dbus package that isn't in systemPackages.
  dbusOnly = mk "dbus-only" ''
    mkdir -p share/dbus-1/system.d
    echo '<busconfig/>' > share/dbus-1/system.d/nbh.conf
  '';
}
