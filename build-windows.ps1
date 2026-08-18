# 在 Windows 上编译 i-agent（需要先装 Rust: https://rustup.rs/）
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

Write-Host "==> 重新生成内嵌资产（技能包 + 浏览器冒烟脚本）" -ForegroundColor Cyan
if (Get-Command node -ErrorAction SilentlyContinue) {
    node scripts/gen_embedded.mjs
} else {
    Write-Host "    未找到 node，跳过资产重新生成（用仓库里现成的 src/embedded.rs）" -ForegroundColor Yellow
}

Write-Host "==> 编译 release" -ForegroundColor Cyan
cargo build --release

$exe = "target\release\i-agent.exe"
if (Test-Path $exe) {
    Write-Host "`n✓ 编译完成: $exe" -ForegroundColor Green
    & $exe -V

    Write-Host "`n提醒：browser 工具需要 Node + Playwright + Chromium：" -ForegroundColor Yellow
    Write-Host "    npm i -g playwright"
    Write-Host "    npx playwright install chromium"
    Write-Host "  没装的话，browser 会明确报错告诉你缺什么——它不会假装验证过。"
} else {
    Write-Host "✗ 没找到产物 $exe" -ForegroundColor Red
    exit 1
}
