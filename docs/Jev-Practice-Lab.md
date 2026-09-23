# Local Jev practice lab

OWASP Juice Shop v20.2.0 runs as `minerva-juice-shop` in Docker inside Ubuntu WSL. It is bound only to 127.0.0.1:3001, with no host volumes, dropped capabilities, no-new-privileges, 768 MiB memory, one CPU, and 200 PIDs. It does not automatically restart after reboot. Docker was installed from Ubuntu's package repository.

From the Minerva folder:

```powershell
./bin/ctf-lab.ps1 up
./bin/ctf-lab.ps1 status
./bin/ctf-lab.ps1 stop
```

Open http://127.0.0.1:3001. In WebUI Graphs, choose `ctf-juice-recon` and Run Jev. The graph collects headers and robots.txt through Eidolon's normal tool dispatcher. It only targets this local lab. Approve its displayed run envelope if prompted; do not disable the dispatcher. No language model is needed for this deterministic smoke test, so it reports no invented confidence score.

Measured first run: `r_a3d745cb534544d7a287`, reached `report`, two real shell actions, path `headers → robots → report`, HTTP 200 and `/ftp` found in robots.txt. Evidence: logs/ctf-lab-smoke.json. This proves WSL shell execution and Jev dispatch, not exploitation coverage or learned decision quality.

This is a practice container inside WSL, not a separately provisioned full VM. Additional challenge images and learned decision scenarios can use the same local-only pattern.

Official setup reference: https://pwning.owasp-juice.shop/companion-guide/latest/part1/running.html
