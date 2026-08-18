// 扫描 assets/ 生成 src/embedded.rs（编译期内嵌资产，单二进制自带全部技能包与工具脚本）
// 用法: node scripts/gen_embedded.mjs
import { readdirSync, statSync, writeFileSync } from 'node:fs';
import { join, relative, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const assets = join(root, 'assets');

const walk = (dir) =>
  readdirSync(dir).flatMap((e) => {
    const p = join(dir, e);
    return statSync(p).isDirectory() ? walk(p) : [p];
  });

const files = walk(assets)
  .map((p) => relative(assets, p).split('\\').join('/'))
  .sort();

const lines = [
  '// 本文件由 scripts/gen_embedded.mjs 自动生成，勿手改',
  'pub static ASSETS: &[(&str, &str)] = &[',
  ...files.map(
    (rel) =>
      `    ("${rel}", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/${rel}"))),`
  ),
  '];',
  '',
];

writeFileSync(join(root, 'src', 'embedded.rs'), lines.join('\n'), 'utf8');
console.log(`已生成 src/embedded.rs（${files.length} 个资产）`);
