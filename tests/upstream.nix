# Fail when nixpkgs changes code that nixos-build-helpers reimplements or relies on.
#
# The differential tests only cover what our test configurations exercise. These hashes make sure
# someone looks at every upstream change of a reimplemented builder: review the change, port it
# (with a test) if needed, then update the hash in ../upstream-code.nix.
#
# The module checks the same at evaluation time, in Nix (../lib/upstream-check.nix), against the
# nixpkgs a system is built with. This checks with sed and grep, which also tests that
# implementation (the test systems are evaluated with it), and searches all of nixos/modules.
{
  lib,
  path,
  runCommand,
}:
let
  code = import ../upstream-code.nix;
  hash = "sha256sum | cut -d' ' -f1";
  check = what: actual: expected: ''
    actual=$(${actual})
    if [ "$actual" != ${expected} ]; then
      echo "CHANGED: ${what}: $actual"
      failed=1
    fi
  '';
  grepUses = "grep -H 'system\\.path\\b'";
in
runCommand "test-upstream" { } ''
  failed=0
  cd ${path}

  ${lib.concatMapStrings (
    r:
    check r.file (
      if r.range == null then
        "sha256sum < ${r.file} | cut -d' ' -f1"
      else
        "sed -E -n ${lib.escapeShellArg "/^${r.range.from}$/,/^${r.range.to}$/p"} ${r.file} | ${hash}"
    ) r.sha256
  ) code.reimplemented}

  grep -qF ${lib.escapeShellArg code.etcInlineContentMax.text} ${code.etcInlineContentMax.file} \
    || { echo "CHANGED: etcInlineContentMax (INLINE_CONTENT_MAX in build_composefs_dump.rs)"; failed=1; }

  # The files the module checks, and all of nixos/modules for uses in other files
  ${check "the uses of system.path in the listed files"
    "${grepUses} ${toString code.systemPathUses.files} | LC_ALL=C sort | ${hash}"
    code.systemPathUses.sha256
  }
  ${check "the uses of system.path in nixos/modules (in a file not listed in upstream-code.nix?)"
    "${grepUses} -r --include='*.nix' nixos/modules | LC_ALL=C sort | ${hash}"
    code.systemPathUses.sha256
  }
  [ $failed = 0 ] || ${grepUses} -r --include='*.nix' nixos/modules | LC_ALL=C sort

  [ $failed = 0 ] && touch $out
''
