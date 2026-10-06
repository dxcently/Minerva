# The installer crate's `minerva` binary, kept as `bin/minerva-core`. The Nix `minerva` command (minerva.sh) calls it for
# the one job bash should not redo: installing, trusting and refreshing the bundled plugins (`plugins sync`), the same code
# the Linux installer runs. It is never used to update (a Nix install updates through nix), so the compiled-in release
# key is the placeholder on purpose: every signature check in it fails closed.
{ lib, rustPlatform, version }:
let
  src = lib.cleanSourceWith {
    name = "minerva-installer-source";
    src = lib.cleanSource ../installer;
    filter = path: type:
      let base = baseNameOf (toString path); in
      !(type == "directory" && (base == "target" || base == "dist" || base == "chromium"));
  };
in
rustPlatform.buildRustPackage {
  pname = "minerva-core";
  inherit version src;
  cargoLock.lockFile = ../installer/Cargo.lock;
  cargoBuildFlags = [ "--bin" "minerva" ];
  # The tests drive a real eidolon and a git; they run in the installer's own checks, not here.
  doCheck = false;
  MINERVA_ALLOW_PLACEHOLDER_KEY = "1";
  MINERVA_VERSION_BASE = version;
  MINERVA_BUILD_SHA = "nix";
  postInstall = "mv $out/bin/minerva $out/bin/minerva-core";
  meta = {
    description = "Minerva installer core: plugin sync for the Nix command";
    license = lib.licenses.mit;
    mainProgram = "minerva-core";
    platforms = lib.platforms.linux;
  };
}
