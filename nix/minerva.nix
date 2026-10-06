# The desktop set: `minerva` (the command) plus eidolon itself. The command refers to perch,
# the wizard, the web UI, eidolon, the bundled plugins and the browser service by store path.
{ lib, stdenvNoCC, makeWrapper, symlinkJoin, bash, coreutils, curl, iproute2, gnugrep, gnused, gawk
, procps, git, eidolon, perch, minerva-setup, webui-term, minerva-core, minerva-plugins, eidolon-browser, version }:
let
  command = stdenvNoCC.mkDerivation {
    pname = "minerva-command";
    inherit version;
    src = ./minerva.sh;
    dontUnpack = true;
    nativeBuildInputs = [ makeWrapper ];
    installPhase = ''
      mkdir -p $out/bin
      substitute $src $out/bin/minerva \
        --subst-var-by version ${version} \
        --subst-var-by perch ${perch} \
        --subst-var-by setup ${minerva-setup} \
        --subst-var-by eidolon ${eidolon} \
        --subst-var-by webui ${webui-term}         --subst-var-by core ${minerva-core}         --subst-var-by plugins ${minerva-plugins}         --subst-var-by browser ${eidolon-browser}
      chmod +x $out/bin/minerva
      patchShebangs $out/bin/minerva
      wrapProgram $out/bin/minerva --prefix PATH : ${lib.makeBinPath [ coreutils curl iproute2 gnugrep gnused gawk procps git ]}
    '';
    meta.mainProgram = "minerva";
  };
in
symlinkJoin {
  name = "minerva-${version}";
  pname = "minerva";
  inherit version;
  paths = [ command eidolon ];
  meta = {
    description = "Minerva desktop set: the minerva command and eidolon";
    mainProgram = "minerva";
    platforms = lib.platforms.linux;
  };
}
