# Compare our buildEnv builder against nixpkgs' builder.pl on edge cases.
{
  lib,
  buildEnv,
  callPackage,
  runCommand,
  writeText,
  fixtures,
  compare,
  exe ? lib.getExe (callPackage ../package.nix { }),
}:
let
  ours =
    env:
    env.overrideAttrs {
      buildCommand = ''
        ${exe} build-env
        eval "$postBuild"
      '';
    };

  only =
    name: file: content:
    runCommand "nbh-fixture-${name}" { }
      "mkdir -p $out/$(dirname ${file}); echo ${content} > $out/${file}";
  single = writeText "nbh-single-file" "x";

  envs = with fixtures; {
    default = {
      paths = buildEnvPaths;
      ignoreCollisions = true;
    };
    reversed = {
      paths = lib.reverseList buildEnvPaths;
      ignoreCollisions = true;
    };
    pathsToLink = {
      paths = buildEnvPaths;
      ignoreCollisions = true;
      pathsToLink = [
        "/bin"
        "/share/x"
        "/share/common"
      ];
      extraPrefix = "/prefix";
    };
    extraOutputs = {
      paths = [ multiOutput ];
      extraOutputsToInstall = [ "man" ];
    };
    sameContents = {
      paths = [
        (only "same-1" "share/same" "same")
        (only "same-2" "share/same" "same")
      ];
    };
    priorities = {
      paths = [
        lowFile
        (only "foo" "bin/foo" "foo")
      ];
    };
    singleFileIgnored = {
      paths = [
        single
        a
      ];
      ignoreSingleFileOutputs = true;
    };
    includeClosures = {
      paths = [ linkDir ];
      includeClosures = true;
    };
    manifest = {
      paths = [ a ];
      manifest = writeText "nbh-manifest" "m";
    };
    postBuild = {
      paths = [ a ];
      postBuild = "echo post > $out/post";
    };
  };

  # Both builders must reject these.
  failing = with fixtures; {
    differentContents.paths = [
      a
      b
    ];
    singleFile.paths = [ single ];
    pathsToLinkFile = {
      paths = [ a ];
      pathsToLink = [ "/bin/foo/x" ];
    };
  };

  mkEnv = name: args: buildEnv ({ name = "nbh-test-env-${name}"; } // args);

  expectFailure =
    env:
    env.overrideAttrs (o: {
      buildCommand = ''
        if (${lib.head (lib.splitString "\n" o.buildCommand)}) 2>err; then
          echo "unexpected success"; exit 1
        fi
        echo "failed as expected: $(tail -n1 err)"
        rm -rf $out; mkdir $out
      '';
    });
in
runCommand "test-build-env" { } ''
  failed=0
  ${lib.concatStrings (
    lib.mapAttrsToList (name: args: ''
      echo "== ${name}"
      ${compare} ${mkEnv name args} ${ours (mkEnv name args)} || failed=1
    '') envs
  )}
  ${lib.concatStrings (
    lib.mapAttrsToList (name: args: ''
      echo "== ${name} (must fail)"
      echo "  reference: ${expectFailure (mkEnv name args)}"
      echo "  ours:      ${expectFailure (ours (mkEnv name args))}"
    '') failing
  )}
  [ $failed = 0 ] && touch $out
''
