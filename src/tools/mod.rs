mod browser;
mod bundle;
mod check;
mod fs_tools;
mod image;
mod jscheck;
mod python;
mod shell;
mod web;
mod xlsx;

pub use shell::shell_name;

use crate::config::Config;
use serde_json::{json, Value};

/// 工具 JSON Schema（描述面向短上下文模型，力求精炼）
pub fn specs(cfg: &Config, depth: u8) -> Value {
    let mut v = vec![
        json!({"type":"function","function":{"name":"read",
            "description":"读文件，带行号。默认 600 行；参考文件一次读够（limit 最大 2000），别分段蚂蚁搬家。",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string","description":"文件路径，相对工作目录或绝对路径"},
                "offset":{"type":"integer","description":"起始行号，从 1 开始"},
                "limit":{"type":"integer","description":"读取行数，默认 600，最大 2000"}
            },"required":["path"]}}}),
        json!({"type":"function","function":{"name":"write",
            "description":"写文件，自动创建父目录。默认覆盖；append=true 追加到末尾。800 行以内一次写完；更大的文件才分多次写（首次覆盖，之后 append 续写，每块 500-800 行）。写成功即落盘，无需回读确认。",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string"},
                "content":{"type":"string"},
                "append":{"type":"boolean","description":"true 时追加"}
            },"required":["path","content"]}}}),
        json!({"type":"function","function":{"name":"edit",
            "description":"精确文本替换。old 必须在文件中唯一出现，否则报错；all=true 替换全部。",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string"},
                "old":{"type":"string","description":"要被替换的原文，须逐字精确"},
                "new":{"type":"string"},
                "all":{"type":"boolean"}
            },"required":["path","old","new"]}}}),
        json!({"type":"function","function":{"name":"ls",
        "description":"列目录内容。",
        "parameters":{"type":"object","properties":{
            "path":{"type":"string","description":"默认工作目录"}
        }}}}),
        json!({"type":"function","function":{"name":"glob",
            "description":"按通配模式找文件，如 **/*.md、src/**/*.rs。按修改时间新在前。",
            "parameters":{"type":"object","properties":{
                "pattern":{"type":"string"},
                "path":{"type":"string","description":"搜索根目录，默认工作目录"}
            },"required":["pattern"]}}}),
        json!({"type":"function","function":{"name":"grep",
            "description":"文件内容正则搜索，返回 文件:行号:内容。用于找设定、查引用、定位代码。",
            "parameters":{"type":"object","properties":{
                "pattern":{"type":"string","description":"正则表达式，加 (?i) 前缀忽略大小写"},
                "path":{"type":"string","description":"搜索根目录或单个文件，默认工作目录"},
                "glob":{"type":"string","description":"文件名过滤，如 *.md"}
            },"required":["pattern"]}}}),
        json!({"type":"function","function":{"name":"shell",
            "description":format!(
                "执行 shell 命令。本机的 shell 是 **{sh}**，命令必须用 {sh} 的语法。\
                 用于装包、git、统计、跑已有脚本。\
                 注意：要跑 Python 代码一律用 python 工具，不要用 shell 套 `python -c \"...\"` \
                 或 heredoc——引号转义是最高频的失败源。",
                sh = shell::shell_name()),
            "parameters":{"type":"object","properties":{
                "cmd":{"type":"string"},
                "timeout_s":{"type":"integer","description":"超时秒数，默认 120"},
                "cwd":{"type":"string"}
            },"required":["cmd"]}}}),
        json!({"type":"function","function":{"name":"python",
            "description":"直接执行 Python 源码——不经过 shell，因此没有任何引号/转义/heredoc 问题。\
                 凡是要跑 Python（openpyxl 生成 xlsx、python-pptx 生成 PPT、数据清洗、回读产物自证）一律用本工具，\
                 严禁用 shell 套 python -c。返回 stdout+stderr。",
            "parameters":{"type":"object","properties":{
                "code":{"type":"string","description":"完整的 Python 源码"},
                "timeout_s":{"type":"integer","description":"超时秒数，默认 180"},
                "cwd":{"type":"string"}
            },"required":["code"]}}}),
        json!({"type":"function","function":{"name":"fetch",
            "description":"抓取网页并提取正文文本（GET）。引用抓取内容写报告时只用页面实际出现的信息与链接，缺的信息就注明未提及，严禁用记忆补全 URL 或数据。",
            "parameters":{"type":"object","properties":{
                "url":{"type":"string"},
                "max_chars":{"type":"integer","description":"默认 8000"}
            },"required":["url"]}}}),
        json!({"type":"function","function":{"name":"check",
            "description":"确定性校验文件（kind 可省略，按扩展名自动判断）。kind=json：严格 JSON 语法校验（报行列号）+ 游戏数据引用完整性检查。kind=html：逐块 JS 语法校验 + 真浏览器执行冒烟。生成 JSON 数据文件后必须 check 一次；HTML 交付前必须 check 一次。",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string"},
                "kind":{"type":"string","description":"json / html，省略即按扩展名自动判断"}
            },"required":["path"]}}}),
        json!({"type":"function","function":{"name":"browser",
        "description":"在真 Chromium 里执行页面。两用法：① path=本地 HTML 文件 → 交付前验收（白屏、运行时异常、DOM 节点、是否用上 AudioContext/动画、点击后界面有无响应、外链依赖、截图）——这是判断页面能不能真的跑起来的唯一标准，任何 HTML 产物交付前必须跑通一次。② url=线上网址 → 打开并抓回渲染后的正文与页面内 <table> 的真实内容：凡数据是 JS 异步注入（直接 GET 源码拿不到的），一律用本工具的 url 模式从渲染后的 DOM 取，严禁凭源码猜或编造。",
        "parameters":{"type":"object","properties":{
            "path":{"type":"string","description":"本地 HTML 文件路径（验收模式）"},
            "url":{"type":"string","description":"线上页面地址（操纵/取数据模式），与 path 二选一"},
            "out":{"type":"string","description":"url 模式下把渲染后的数据直接落盘：.csv 存第一张表、.json 存结构化正文+全部表、其它存正文文本。落盘后即可直接引用，无需再用 fetch/curl/playwright 核对"},
            "clicks":{"type":"integer","description":"自动点击多少个可点元素来验证交互，默认 3"},
            "wait_ms":{"type":"integer","description":"加载后等待毫秒数，默认 1200，JS 延迟注入数据的页要调大"}
        }}}}),
        json!({"type":"function","function":{"name":"bundle",
            "description":"把多段文件按顺序确定性地拼成一个产物（如 引擎头 + 数据 + 引擎尾 = game.html）。会自动按 JS 规则在接缝处补分号，并逐块做真语法检查。拼接 HTML 一律用本工具，严禁用 shell 的 cat/Get-Content 手工拼——接缝漏一个分号就整页白屏。",
            "parameters":{"type":"object","properties":{
                "out":{"type":"string","description":"输出文件路径，如 game.html"},
                "parts":{"type":"array","items":{"type":"string"},"description":"按拼接顺序排列的文件路径"}
            },"required":["out","parts"]}}}),
        json!({"type":"function","function":{"name":"xlsx",
            "description":"把 CSV 转成 Excel .xlsx（可多表）。",
            "parameters":{"type":"object","properties":{
                "sources":{"type":"array","items":{"type":"object","properties":{
                    "csv":{"type":"string","description":"CSV 文件路径"},
                    "name":{"type":"string","description":"工作表名"}},"required":["csv"]}},
                "out":{"type":"string","description":"输出 .xlsx 路径"}
            },"required":["sources","out"]}}}),
    ];
    if !cfg.image_providers.is_empty() {
        v.push(json!({"type":"function","function":{"name":"image",
            "description":"AI 文生图并保存。prompt 描述画面内容与统一风格；同一产物内多张图应使用相同风格词缀。工具会按供应商返回的真实格式确定扩展名（可能是 jpg），以返回消息里的最终路径为准去引用，严禁事后改扩展名伪装格式；若与用户要求的扩展名不同，在交付说明中注明即可。",
            "parameters":{"type":"object","properties":{
                "prompt":{"type":"string"},
                "path":{"type":"string","description":"保存路径，如 assets/img/cover.png"},
                "size":{"type":"string","description":"如 1024x1024、1344x768(横)、768x1344(竖)，默认 1024x1024"}
            },"required":["prompt","path"]}}}));
    }
    if depth == 0 {
        v.push(json!({"type":"function","function":{"name":"task",
            "description":"派一个子 agent 独立完成子任务，返回其结果摘要。prompt 必须自包含：背景、具体要求、输出文件路径。一条消息里多个 task 会并行执行，适合分章写作、批量生成。",
            "parameters":{"type":"object","properties":{
                "prompt":{"type":"string"},
                "label":{"type":"string","description":"任务短名"}
            },"required":["prompt"]}}}));
    }
    Value::Array(v)
}

pub fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

pub fn dispatch(name: &str, args: &Value, cfg: &Config) -> Result<String, String> {
    match name {
        "read" => fs_tools::read(args, cfg),
        "write" => fs_tools::write(args, cfg),
        "edit" => fs_tools::edit(args, cfg),
        "ls" => fs_tools::ls(args, cfg),
        "glob" => fs_tools::glob(args, cfg),
        "grep" => fs_tools::grep(args, cfg),
        "shell" => shell::run(args, cfg),
        "python" => python::run(args, cfg),
        "check" => check::run(args, cfg),
        "browser" => browser::run(args, cfg),
        "bundle" => bundle::run(args, cfg),
        "fetch" => web::fetch(args, cfg),
        "image" => image::generate(args, cfg),
        "xlsx" => xlsx::convert(args, cfg),
        _ => Err(format!("未知工具 {name}")),
    }
}

/// 头尾截断，防止单条工具输出撑爆短上下文
pub fn cap(text: String, max_chars: usize) -> String {
    let n = text.chars().count();
    if n <= max_chars {
        return text;
    }
    let head: String = text.chars().take(max_chars * 2 / 3).collect();
    let tail: String = text.chars().skip(n - max_chars / 3).collect();
    format!("{head}\n…[中间截断，共 {n} 字符]…\n{tail}")
}
