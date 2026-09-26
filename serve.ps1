# Start llama-server in router mode: every entry in models.ini shows up on
# GET /v1/models, in the built-in WebUI picker, and to any OpenAI-compatible client.
# The bare-bones twin of `hoot serve` (bin/hoot.ps1); the binaries come from
# `hoot setup` (bin/setup-models.ps1, upstream ggml-org/llama.cpp win-vulkan-x64).
param([int]$Port = 8080, [string]$HostAddr = '127.0.0.1')

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot

# models.ini names its files relative to the router's working directory,
# so run from the repo root whatever directory this was started in.
Push-Location $root
try {
    & (Join-Path $root 'llama.cpp\llama-server.exe') `
        --models-preset (Join-Path $root 'models.ini') `
        --models-max 2 `
        --host $HostAddr --port $Port
} finally { Pop-Location }
