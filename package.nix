{ lib, rustPlatform }:
rustPlatform.buildRustPackage {
  pname = "nixos-build-helpers";
  version = "0.1";
  cargoLock.lockFile = ./Cargo.lock;
  src = lib.cleanSource ./.;

  # preCheck = ''
  #   cargo clippy -- -Dwarnings
  # '';

  meta = {
    description = "Helpers for faster builds of NixOS systems";
    mainProgram = "nixos-build-helpers";
    # maintainers = [ lib.maintainers.lubsch ];
    license = lib.licenses.mit;
  };
}
