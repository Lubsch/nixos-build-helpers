# Fail when nixpkgs changes code that nixos-build-helpers reimplements or relies on.
#
# The differential tests only cover what our test configurations exercise. These hashes make sure
# someone looks at every upstream change of a reimplemented builder: review the change, port it
# (with a test) if needed, then update the hash.
{
  path,
  runCommand,
}:
let
  # file -> sed range (or null for the whole file) -> sha256
  reimplemented = [
    {
      file = "pkgs/build-support/buildenv/builder.pl";
      range = null;
      sha256 = "71e762e76f4efc9178dc2048e3e418811f3cb521e9e80321eb36f9091623eb82";
    }
    {
      file = "nixos/modules/system/etc/build-composefs-dump.py";
      range = null;
      sha256 = "440c68d7363f2e9be32d3860ef1c48049722108ac8cffb97662944d06cb50c71";
    }
    {
      file = "nixos/lib/systemd-lib.nix";
      range = "/^  generateUnits =/,/^  makeJobScript =/";
      sha256 = "04ab63fd88682f36f4c81b78dbe7c0d98ddcc50ae53cba3454e60230d66af74f";
    }
    {
      file = "nixos/modules/system/etc/etc.nix";
      range = "/^  etc =$/,/^  etcHardlinks =/";
      sha256 = "482d461bba6a0a21e513228fefff11ebc6afcd014bcf6842fa253842600e78f1";
    }
  ];

in
runCommand "test-upstream" { } ''
  failed=0
  cd ${path}

  ${builtins.concatStringsSep "\n" (
    map (r: ''
      actual=$(${
        if r.range == null then "cat ${r.file}" else "sed -n '${r.range}p' ${r.file}"
      } | sha256sum | cut -d' ' -f1)
      if [ "$actual" != ${r.sha256} ]; then
        echo "CHANGED: ${r.file} ${if r.range == null then "" else "(${r.range})"}: $actual"
        failed=1
      fi
    '') reimplemented
  )}

  grep -q 'etcInlineContentMax = 4096;' nixos/modules/system/etc/etc.nix \
    || { echo "CHANGED: etcInlineContentMax (INLINE_CONTENT_MAX in build_composefs_dump.rs)"; failed=1; }

  [ $failed = 0 ] && touch $out
''
