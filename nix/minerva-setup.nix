{ lib, rustPlatform, version }:
rustPlatform.buildRustPackage {
  pname = "minerva-setup";
  inherit version;
  src = lib.cleanSourceWith {
    name = "minerva-setup-source";
    src = lib.cleanSource ../setup;
    filter = path: type:
      let base = baseNameOf (toString path); in
      !(type == "directory" && (base == "target" || base == "result"));
  };
  cargoLock.lockFile = ../setup/Cargo.lock;
  meta = {
    description = "The Minerva first-run wizard";
    license = lib.licenses.mit;
    mainProgram = "minerva-setup";
    platforms = lib.platforms.unix;
  };
}
