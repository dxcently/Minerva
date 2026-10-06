# $out/bin/minerva-eidolon: execs the eidolon store path, arguments passed through, no --config baked in.
# The real binary is not exported as `eidolon`, so it cannot shadow another eidolon launcher on the host.
{ writeShellScriptBin, eidolon }:
writeShellScriptBin "minerva-eidolon" ''
  exec ${eidolon}/bin/eidolon "$@"
''
