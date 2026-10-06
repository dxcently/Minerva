{ lib, stdenvNoCC, version }:
stdenvNoCC.mkDerivation {
  pname = "minerva-webui-term";
  inherit version;
  src = lib.cleanSourceWith {
    name = "webui-term-source";
    src = lib.cleanSource ../webui/term;
    filter = path: type: baseNameOf (toString path) != "node_modules";
  };
  dontBuild = true;
  installPhase = ''
    mkdir -p $out/share/minerva/webui-term
    cp -r . $out/share/minerva/webui-term
  '';
}
