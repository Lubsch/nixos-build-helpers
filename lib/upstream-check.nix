# The checks of ../upstream-code.nix at evaluation time, against the nixpkgs a system is built with.
# Returns a message per change that concerns an enabled option.
{
  lib,
  # Root of the nixpkgs that pkgs/… resp. nixos/… files are read from: a system's modules and its
  # pkgs can come from different checkouts.
  pkgsRoot,
  nixosRoot,
  # option name -> bool
  enabled,
  # the texinfo whose install-info system.path runs
  texinfo,
}:
let
  code = import ../upstream-code.nix;

  pathOf = file: (if lib.hasPrefix "pkgs/" file then pkgsRoot else nixosRoot) + "/${file}";
  lines =
    text:
    let
      l = lib.splitString "\n" text;
    in
    if l != [ ] && lib.last l == "" then lib.init l else l;
  withNewlines = lib.concatMapStrings (l: l + "\n");

  # sed -E -n '/^from$/,/^to$/p': `to` is looked for from the line after `from`, and a range can
  # start again after it ended.
  inRange =
    { from, to }:
    text:
    (lib.foldl' (
      acc: line:
      if acc.inside then
        {
          inside = builtins.match to line == null;
          out = acc.out ++ [ line ];
        }
      else if builtins.match from line != null then
        {
          inside = true;
          out = acc.out ++ [ line ];
        }
      else
        acc
    ) { inside = false; out = [ ]; } (lines text)).out;

  hashOf =
    { file, range, ... }:
    let
      path = pathOf file;
    in
    if !builtins.pathExists path then
      null
    else if range == null then
      builtins.hashFile "sha256" path
    else
      builtins.hashString "sha256" (withNewlines (inRange range (builtins.readFile path)));

  describe = r: r.file + lib.optionalString (r.range != null) " (from `${r.range.from}` to `${r.range.to}`)";
  any = lib.any (o: enabled.${o} or false);

  reimplemented = map (r: {
    options = r.options;
    changed = hashOf r != r.sha256;
    what = describe r;
  }) code.reimplemented;

  inlineMax = with code.etcInlineContentMax; {
    options = [ "etcOverlay" ];
    changed = !(builtins.pathExists (pathOf file) && lib.hasInfix text (builtins.readFile (pathOf file)));
    what = "${file} (`${text}`, INLINE_CONTENT_MAX in build_composefs_dump.rs)";
  };

  texinfoChanged = {
    options = [ "buildEnv" ];
    changed =
      texinfo.version != code.texinfo.version || map toString (texinfo.patches or [ ]) != code.texinfo.patches;
    what = "texinfo (${texinfo.version}), whose install-info src/install_info.rs reimplements";
  };

  # grep -H 'system\.path\b' <files> | LC_ALL=C sort
  systemPathUses = with code.systemPathUses; {
    options = [ "fixDependencies" ];
    changed =
      let
        matches =
          file:
          let
            path = pathOf file;
          in
          if builtins.pathExists path then
            map (l: "${file}:${l}") (
              lib.filter (l: builtins.match ".*system\\.path([^[:alnum:]_].*)?" l != null) (
                lines (builtins.readFile path)
              )
            )
          else
            [ "${file}: missing" ];
      in
      builtins.hashString "sha256" (withNewlines (lib.sort (a: b: a < b) (lib.concatMap matches files)))
      != sha256;
    what = "the uses of system.path in nixos/modules";
  };
in
map (c: "nixosBuildHelpers.${lib.concatStringsSep "/" c.options}: nixpkgs changed ${c.what}, which nixos-build-helpers reimplements or relies on.") (
  lib.filter (c: any c.options && c.changed) (reimplemented ++ [ inlineMax texinfoChanged systemPathUses ])
)
