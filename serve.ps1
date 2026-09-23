# Start llama-server in router mode: every entry in models.ini shows up on
# GET /v1/models, in the built-in WebUI picker, and to any OpenAI-compatible client.
param([int]$Port = 8080, [string]$HostAddr = '127.0.0.1')

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot

& (Join-Path $root 'llama.cpp\llama-server.exe') `
    --models-preset (Join-Path $root 'models.ini') `
    --models-max 2 `
    --host $HostAddr --port $Port
