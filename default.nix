{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "ai-quotas";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
    ];
  };

  cargoLock.lockFile = ./Cargo.lock;

  cargoBuildFlags = [
    "--bin"
    "ai-quotas"
  ];

  meta = {
    description = "Monitor quotas, costs, and prepaid balances for AI services";
    homepage = "https://github.com/rksm/ai-quotas";
    license = lib.licenses.mit;
    mainProgram = "ai-quotas";
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
  };
}
