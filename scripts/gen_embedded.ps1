# 扫描 assets/ 生成 src/embedded.rs（编译期内嵌技能包，单二进制可携带全部资产）
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$assets = Join-Path $root "assets"
$out = Join-Path $root "src\embedded.rs"
$lines = [System.Collections.Generic.List[string]]::new()
$lines.Add("// 本文件由 scripts/gen_embedded 生成，勿手改")
$lines.Add('pub static ASSETS: &[(&str, &str)] = &[')
Get-ChildItem $assets -Recurse -File | Sort-Object FullName | ForEach-Object {
    $rel = $_.FullName.Substring($assets.Length + 1).Replace('\', '/')
    $lines.Add('    (')
    $lines.Add("        `"$rel`",")
    $lines.Add('        include_str!(concat!(')
    $lines.Add('            env!("CARGO_MANIFEST_DIR"),')
    $lines.Add("            `"/assets/$rel`"")
    $lines.Add('        )),')
    $lines.Add('    ),')
}
$lines.Add('];')
$lines.Add('')
[System.IO.File]::WriteAllText($out, ($lines -join "`n"), [System.Text.UTF8Encoding]::new($false))
Write-Host "已生成 $out（$((Get-ChildItem $assets -Recurse -File).Count) 个资产）"
