{ lib, rustPlatform }:
rustPlatform.buildRustPackage {
  pname = "nixos-build-helpers";
  version = "0.1";
  cargoLock.lockFile = ./Cargo.lock;
  src = builtins.filterSource (name: _: !(lib.hasSuffix ".nix" name)) ./.;

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
