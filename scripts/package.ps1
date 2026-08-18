# i-agent 一键打包：内嵌资产 -> release 构建 -> dist zip
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "未找到 cargo；请先安装 Rust 并确保 cargo 在 PATH 中"
}

& "$PSScriptRoot\gen_embedded.ps1"
cargo test --release
if ($LASTEXITCODE -ne 0) { throw "测试失败" }
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "构建失败" }

$ver = (Select-String -Path "$root\Cargo.toml" -Pattern 'version = "(.+)"').Matches[0].Groups[1].Value
$dist = Join-Path $root "dist"
$stage = Join-Path $dist "i-agent-$ver-win-x64"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force -Confirm:$false }
New-Item -ItemType Directory -Force $stage | Out-Null

Copy-Item "$root\target\release\i-agent.exe" $stage
Copy-Item "$root\README.md" $stage
Copy-Item "$root\Dockerfile" $stage
Copy-Item "$root\assets" "$stage\assets" -Recurse # 备查/可改；二进制内已内嵌同一份
Copy-Item "$root\config.example.json" "$stage\config.example.json"

$zip = Join-Path $dist "i-agent-$ver-win-x64.zip"
if (Test-Path $zip) { Remove-Item $zip -Force -Confirm:$false }
Compress-Archive -Path "$stage\*" -DestinationPath $zip
$binMB = [Math]::Round((Get-Item "$root\target\release\i-agent.exe").Length / 1MB, 2)
$zipMB = [Math]::Round((Get-Item $zip).Length / 1MB, 2)
Write-Host "打包完成: $zip"
Write-Host "二进制 $binMB MB | zip $zipMB MB"
