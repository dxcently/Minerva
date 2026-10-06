{ lib, rustPlatform, runCommand, harnoxSrc, version, doCheck ? false }:
let
  perchSrc = lib.cleanSourceWith {
    name = "perch-source";
    src = lib.cleanSource ../perch;
    filter = path: type:
      let base = baseNameOf (toString path); in
      !(type == "directory" && (base == "target" || base == "result"));
  };
  # perch/Cargo.toml says `harnox = { path = "../harnox" }`: restore the sibling layout.
  workspace = runCommand "perch-workspace" { } ''
    mkdir -p $out
    cp -r ${perchSrc} $out/perch
    cp -r ${harnoxSrc} $out/harnox
    chmod -R u+w $out
  '';
in
rustPlatform.buildRustPackage {
  pname = "perch";
  inherit version doCheck;
  src = workspace;
  sourceRoot = "perch-workspace/perch";
  cargoLock.lockFile = ../perch/Cargo.lock;
  # The tests drive src/bin/fake_eidolon.rs, a test-only target.
  buildFeatures = lib.optional doCheck "test-bins";
  checkFeatures = [ "test-bins" ];
  # The perch needs an existing $HOME (default root); the sandbox points it at a missing dir.
  preCheck = "export HOME=$TMPDIR/home; mkdir -p $HOME";
  meta = {
    description = "The Minerva perch: one loopback HTTP server in front of several eidolon web doors";
    license = lib.licenses.mit;
    mainProgram = "perch";
    platforms = lib.platforms.linux;
  };
}
