param([ValidateSet('up','stop','status')][string]$Action='status')
$ErrorActionPreference='Stop'
$name='minerva-juice-shop'
if($Action -eq 'status') { & wsl -d Ubuntu -u root -- docker ps -a --filter "name=^/$name$" --format '{{.Names}} {{.Status}} {{.Ports}}'; exit $LASTEXITCODE }
if($Action -eq 'stop') { & wsl -d Ubuntu -u root -- docker stop $name; exit $LASTEXITCODE }
& wsl -d Ubuntu -u root -- docker inspect $name *> $null
if($LASTEXITCODE -eq 0) { & wsl -d Ubuntu -u root -- docker start $name }
else { & wsl -d Ubuntu -u root -- docker run -d --name $name --restart no --memory 768m --cpus 1 --pids-limit 200 --cap-drop ALL --security-opt no-new-privileges:true -p 127.0.0.1:3001:3000 bkimminich/juice-shop:v20.2.0 }
if($LASTEXITCODE -ne 0) { throw 'Juice Shop did not start' }
Write-Output 'Juice Shop: http://127.0.0.1:3001 | Jev graph: ctf-juice-recon'
