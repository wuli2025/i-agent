# 扫描 assets/ 生成 src/embedded.rs（编译期内嵌技能包，单二进制可携带全部资产）
$root = Split-Path $PSScriptRoot -Parent
$assets = Join-Path $root "assets"
$out = Join-Path $root "src\embedded.rs"
$lines = @("// 本文件由 scripts/gen_embedded.ps1 自动生成，勿手改", "pub static ASSETS: &[(&str, &str)] = &[")
Get-ChildItem $assets -Recurse -File | Sort-Object FullName | ForEach-Object {
    $rel = $_.FullName.Substring($assets.Length + 1).Replace('\', '/')
    $lines += "    (`"$rel`", include_str!(concat!(env!(`"CARGO_MANIFEST_DIR`"), `"/assets/$rel`"))),"
}
$lines += "];"
Set-Content -Path $out -Value ($lines -join "`n") -Encoding utf8
Write-Host "已生成 $out（$((Get-ChildItem $assets -Recurse -File).Count) 个资产）"
