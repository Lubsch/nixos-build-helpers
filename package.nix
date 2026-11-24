{ lib, rustPlatform }:
rustPlatform.buildRustPackage {
  pname = "nixos-build-helpers";
  version = "0.1";
  cargoLock.lockFile = ./Cargo.lock;
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
    ];
  };

  meta = {
    description = "Faster builders for parts of NixOS systems";
    mainProgram = "nixos-build-helpers";
    license = lib.licenses.mit;
  };
}
