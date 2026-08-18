//! CSV -> XLSX 转换（零依赖：手写 CSV 解析 + ZIP(STORED) + SpreadsheetML）
use super::arg_str;
use crate::config::Config;
use serde_json::Value;

pub fn convert(args: &Value, cfg: &Config) -> Result<String, String> {
    let sources = args
        .get("sources")
        .and_then(|v| v.as_array())
        .ok_or("缺少 sources")?;
    let out = arg_str(args, "out").ok_or("缺少 out")?;
    if sources.is_empty() {
        return Err("sources 为空".into());
    }
    let mut sheets: Vec<(String, Vec<Vec<String>>)> = Vec::new();
    for (i, src) in sources.iter().enumerate() {
        let csv_path = src
            .get("csv")
            .and_then(|v| v.as_str())
            .ok_or("source 缺少 csv")?;
        let name = src
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("Sheet{}", i + 1));
        let p = cfg.resolve(csv_path);
        let bytes = std::fs::read(&p).map_err(|e| format!("读取 {csv_path} 失败: {e}"))?;
        let text = String::from_utf8_lossy(&bytes);
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let rows = parse_csv(text);
        if rows.len() > 100_000 {
            return Err(format!("{csv_path} 超过 10 万行，请拆分"));
        }
        sheets.push((sanitize_sheet_name(&name), rows));
    }

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut ct = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>"#,
    );
    for i in 0..sheets.len() {
        ct.push_str(&format!(
            r#"<Override PartName="/xl/worksheets/sheet{}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#,
            i + 1
        ));
    }
    ct.push_str("</Types>");
    files.push(("[Content_Types].xml".into(), ct.into_bytes()));

    files.push((
        "_rels/.rels".into(),
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#
            .to_vec(),
    ));

    let mut wb = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>"#,
    );
    let mut wbr = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    for (i, (name, _)) in sheets.iter().enumerate() {
        wb.push_str(&format!(
            r#"<sheet name="{}" sheetId="{}" r:id="rId{}"/>"#,
            xml_escape(name),
            i + 1,
            i + 1
        ));
        wbr.push_str(&format!(
            r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{}.xml"/>"#,
            i + 1,
            i + 1
        ));
    }
    wb.push_str("</sheets></workbook>");
    wbr.push_str("</Relationships>");
    files.push(("xl/workbook.xml".into(), wb.into_bytes()));
    files.push(("xl/_rels/workbook.xml.rels".into(), wbr.into_bytes()));

    for (i, (_, rows)) in sheets.iter().enumerate() {
        let mut sx = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#,
        );
        for (r, row) in rows.iter().enumerate() {
            sx.push_str(&format!("<row r=\"{}\">", r + 1));
            for (c, cell) in row.iter().enumerate() {
                let cell_ref = format!("{}{}", col_name(c), r + 1);
                let trimmed = cell.trim();
                if !trimmed.is_empty() && trimmed.parse::<f64>().is_ok() && trimmed.len() < 16 {
                    sx.push_str(&format!("<c r=\"{cell_ref}\"><v>{trimmed}</v></c>"));
                } else if !cell.is_empty() {
                    sx.push_str(&format!(
                        "<c r=\"{cell_ref}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
                        xml_escape(cell)
                    ));
                }
            }
            sx.push_str("</row>");
        }
        sx.push_str("</sheetData></worksheet>");
        files.push((format!("xl/worksheets/sheet{}.xml", i + 1), sx.into_bytes()));
    }

    let zip = build_zip(&files);
    let out_path = cfg.resolve(out);
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&out_path, &zip).map_err(|e| e.to_string())?;
    Ok(format!(
        "已生成 {out}（{} 个工作表，{} 字节）",
        sheets.len(),
        zip.len()
    ))
}

fn sanitize_sheet_name(name: &str) -> String {
    let bad = ['\\', '/', '?', '*', '[', ']', ':'];
    let s: String = name.chars().filter(|c| !bad.contains(c)).take(31).collect();
    if s.is_empty() {
        "Sheet".into()
    } else {
        s
    }
}

fn col_name(mut idx: usize) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (idx % 26) as u8) as char);
        if idx < 26 {
            break;
        }
        idx = idx / 26 - 1;
    }
    s
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                _ => field.push(c),
            }
        } else {
            match c {
                '"' => in_quotes = true,
                ',' => {
                    row.push(std::mem::take(&mut field));
                }
                '\r' => {}
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                _ => field.push(c),
            }
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

// ---- 最小 ZIP 写出（STORED 无压缩） ----

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_roundtrip() {
        let rows = parse_csv("姓名,年龄,备注\n\"张三\",30,\"说了\"\"你好\"\"\"\n李四,25.5,多行\n");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1], vec!["张三", "30", "说了\"你好\""]);
    }

    #[test]
    fn zip_structure() {
        let files = vec![("a.xml".to_string(), b"<x/>".to_vec())];
        let z = build_zip(&files);
        assert_eq!(&z[0..4], &[0x50, 0x4B, 0x03, 0x04]); // local header
        let eocd = z.len() - 22;
        assert_eq!(&z[eocd..eocd + 4], &[0x50, 0x4B, 0x05, 0x06]);
    }

    #[test]
    fn write_real_file() {
        let dir = std::env::temp_dir().join("ia_xlsx_test");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("t.csv"),
            "品名,单价,数量\n苹果,3.5,10\n梨,2,\"5\"\n",
        )
        .unwrap();
        let cfg = crate::config::Config {
            workspace: dir.clone(),
            provider: crate::config::Provider {
                name: "t".into(),
                base: String::new(),
                model: String::new(),
                key: String::new(),
                protocol: crate::config::detect_protocol("", ""),
            },
            fallbacks: vec![],
            image_providers: vec![],
            context_window: 32768,
            max_output: 4096,
            max_turns: 8,
            assets_dir: dir.clone(),
            quiet: true,
            canvas_url: "http://127.0.0.1:8787".into(),
            canvas_id: "main".into(),
        };
        let args = serde_json::json!({
            "sources": [{"csv": "t.csv", "name": "库存"}],
            "out": "t.xlsx"
        });
        let r = convert(&args, &cfg).unwrap();
        assert!(r.contains("已生成"));
        assert!(dir.join("t.xlsx").exists());
    }

    #[test]
    fn col_names() {
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(25), "Z");
        assert_eq!(col_name(26), "AA");
        assert_eq!(col_name(27), "AB");
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for i in 0..256u32 {
        let mut c = i;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        table[i as usize] = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

fn put_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn put_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn build_zip(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    let mut count = 0u16;
    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let name_bytes = name.as_bytes();
        // local file header
        put_u32(&mut out, 0x0403_4B50);
        put_u16(&mut out, 20);
        put_u16(&mut out, 0x0800); // UTF-8 名称
        put_u16(&mut out, 0); // stored
        put_u16(&mut out, 0);
        put_u16(&mut out, 0x21); // 日期占位
        put_u32(&mut out, crc);
        put_u32(&mut out, data.len() as u32);
        put_u32(&mut out, data.len() as u32);
        put_u16(&mut out, name_bytes.len() as u16);
        put_u16(&mut out, 0);
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);
        // central directory
        put_u32(&mut central, 0x0201_4B50);
        put_u16(&mut central, 20);
        put_u16(&mut central, 20);
        put_u16(&mut central, 0x0800);
        put_u16(&mut central, 0);
        put_u16(&mut central, 0);
        put_u16(&mut central, 0x21);
        put_u32(&mut central, crc);
        put_u32(&mut central, data.len() as u32);
        put_u32(&mut central, data.len() as u32);
        put_u16(&mut central, name_bytes.len() as u16);
        put_u16(&mut central, 0);
        put_u16(&mut central, 0);
        put_u16(&mut central, 0);
        put_u16(&mut central, 0);
        put_u32(&mut central, 0);
        put_u32(&mut central, offset);
        central.extend_from_slice(name_bytes);
        count += 1;
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    put_u32(&mut out, 0x0605_4B50);
    put_u16(&mut out, 0);
    put_u16(&mut out, 0);
    put_u16(&mut out, count);
    put_u16(&mut out, count);
    put_u32(&mut out, cd_size);
    put_u32(&mut out, cd_offset);
    put_u16(&mut out, 0);
    out
}
