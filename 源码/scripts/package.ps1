# i-agent 一键打包：内嵌资产 -> release 构建 -> dist zip
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
$env:PATH = "C:\Users\mi\.cargo\bin;$env:PATH"

& "$PSScriptRoot\gen_embedded.ps1"
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "构建失败" }
cargo test --release 2>&1 | Select-Object -Last 3

$ver = (Select-String -Path "$root\Cargo.toml" -Pattern 'version = "(.+)"').Matches[0].Groups[1].Value
$dist = Join-Path $root "dist"
$stage = Join-Path $dist "i-agent-$ver-win-x64"
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null

# README/Dockerfile/config.example.json 在仓库根（源码目录的上一层）
$repo = Split-Path $root -Parent
Copy-Item "$root\target\release\i-agent.exe" $stage
Copy-Item "$repo\README.md" $stage
Copy-Item "$repo\Dockerfile" $stage
Copy-Item "$root\assets" "$stage\assets" -Recurse   # 备查/可改；二进制内已内嵌同一份
Copy-Item "$repo\config.example.json" "$stage\config.example.json"

$zip = Join-Path $dist "i-agent-$ver-win-x64.zip"
Remove-Item $zip -Force -ErrorAction SilentlyContinue
Compress-Archive -Path "$stage\*" -DestinationPath $zip
$binMB = [Math]::Round((Get-Item "$root\target\release\i-agent.exe").Length / 1MB, 2)
$zipMB = [Math]::Round((Get-Item $zip).Length / 1MB, 2)
Write-Host "打包完成: $zip"
Write-Host "二进制 $binMB MB | zip $zipMB MB"
