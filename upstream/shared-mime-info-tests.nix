# Upstream's test suite of shared-mime-info (with xdgmime), with or without the patch:
#   nix build --impure --expr 'import ./upstream/shared-mime-info-tests.nix { ref = "refs/tags/2.4"; patched = true; }'
{ ref, patched }:
let
  pkgs = import (builtins.getFlake (toString ../.)).inputs.nixpkgs { };
  src = builtins.fetchGit {
    url = "https://gitlab.freedesktop.org/xdg/shared-mime-info.git";
    inherit ref;
    submodules = true;
  };
  xdgmime = builtins.fetchGit {
    url = "https://gitlab.freedesktop.org/xdg/xdgmime.git";
    ref = "master";
  };
  isMaster = ref == "master";
in
pkgs.shared-mime-info.overrideAttrs (o: {
  name = "shared-mime-info-tests-${if patched then "patched" else "original"}-${builtins.replaceStrings [ "/" ] [ "-" ] ref}";
  inherit src;
  patches = pkgs.lib.optional patched ./shared-mime-info-comment-lookup.patch;
  nativeBuildInputs = o.nativeBuildInputs ++ [ pkgs.xmlto pkgs.docbook-xsl-nons pkgs.docbook_xml_dtd_412 ];
  nativeCheckInputs = with pkgs; [ python3 libxml2 ];
  doCheck = true;
  postPatch = (o.postPatch or "") + ''
    patchShebangs .
  '' + pkgs.lib.optionalString isMaster ''
    cp -r ${xdgmime} subprojects/xdgmime
    chmod -R u+w subprojects/xdgmime
  '';
  # 2.4 takes the path of built xdgmime test programs, in <path>/src
  preConfigure = (o.preConfigure or "") + pkgs.lib.optionalString (!isMaster) ''
    cp -r ${xdgmime} $NIX_BUILD_TOP/xdgmime-src
    chmod -R u+w $NIX_BUILD_TOP/xdgmime-src
    meson setup $NIX_BUILD_TOP/xdgmime-build $NIX_BUILD_TOP/xdgmime-src
    ninja -C $NIX_BUILD_TOP/xdgmime-build
    mkdir -p $NIX_BUILD_TOP/xdgmime/src
    ln -s $NIX_BUILD_TOP/xdgmime-build/src/print-mime-data $NIX_BUILD_TOP/xdgmime/src/
    ln -s $NIX_BUILD_TOP/xdgmime-build/src/tests/test-mime-data $NIX_BUILD_TOP/xdgmime/src/
    ln -s $NIX_BUILD_TOP/xdgmime-build/src/tests/test-mime $NIX_BUILD_TOP/xdgmime/src/
    mesonFlagsArray+=(-Dxdgmime-path=$NIX_BUILD_TOP/xdgmime)
  '';
  checkPhase = ''
    runHook preCheck
    meson test --print-errorlogs --no-rebuild 2>&1 | tee $NIX_BUILD_TOP/test.log
    runHook postCheck
  '';
  postInstall = ''
    cp $NIX_BUILD_TOP/test.log $out/test.log
  '';
})
