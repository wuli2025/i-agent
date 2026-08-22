use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Protocol {
    OpenAI,
    Anthropic,
}

#[derive(Clone, Debug)]
pub struct Provider {
    pub name: String,
    pub base: String,
    pub model: String,
    pub key: String,
    pub protocol: Protocol,
}

/// 协议判定：显式声明 > 从 base 里认出 /anthropic 端点 > 默认 OpenAI 兼容
pub fn detect_protocol(explicit: &str, base: &str) -> Protocol {
    match explicit.trim().to_lowercase().as_str() {
        "anthropic" | "claude" | "messages" => Protocol::Anthropic,
        "openai" | "oai" | "chat" => Protocol::OpenAI,
        _ => {
            if base.contains("/anthropic") || base.contains("api.anthropic.com") {
                Protocol::Anthropic
            } else {
                Protocol::OpenAI
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct ImageProvider {
    pub name: String,
    pub base: String,
    pub model: String,
    pub key: String,
}

pub struct Config {
    pub workspace: PathBuf,
    pub provider: Provider,
    pub fallbacks: Vec<Provider>,
    pub image_providers: Vec<ImageProvider>,
    pub context_window: usize,
    pub max_output: usize,
    pub max_turns: usize,
    pub assets_dir: PathBuf,
    pub quiet: bool,
    /// Polaris stdio 模式下不在用户工作目录写会话或工具临时状态。
    pub stateless: bool,
    /// AI Canvas Agent API 地址（默认本机 8787）。
    pub canvas_url: String,
    /// 未在工具调用中指定时使用的画布 ID。
    pub canvas_id: String,
}

#[derive(Deserialize, Default)]
struct FileProvider {
    name: String,
    base: String,
    model: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    key_env: String,
    /// "openai"（默认）或 "anthropic"；省略时按 base 自动判断
    #[serde(default)]
    protocol: String,
}

#[derive(Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    context_window: Option<usize>,
    #[serde(default)]
    max_output: Option<usize>,
    #[serde(default)]
    max_turns: Option<usize>,
    #[serde(default)]
    providers: Vec<FileProvider>,
    #[serde(default)]
    image_providers: Vec<FileProvider>,
    #[serde(default)]
    canvas_url: Option<String>,
    #[serde(default)]
    canvas_id: Option<String>,
}

pub fn home_dir() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn env_nonempty(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Some(v.trim().to_string()),
        _ => None,
    }
}

fn resolve_key(direct: &str, key_env: &str) -> String {
    if !direct.is_empty() {
        return direct.to_string();
    }
    if !key_env.is_empty() {
        return env_nonempty(key_env).unwrap_or_default();
    }
    String::new()
}

/// 内置国产供应商。能走 Anthropic 协议的一律走 Anthropic 端点：
/// 服务端把推理内容剥离到独立 block，content 不混 <think>，长任务更稳。
/// 端点/模型名以 2026-07 实测为准（kimi=api.kimi.com/coding 双协议同前缀，模型 k3；
/// minimax=api.minimaxi.com/anthropic，模型 MiniMax-M3）。
fn builtin_providers() -> Vec<(
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
)> {
    // name, base, default model, protocol, key envs（按序取第一个非空）
    vec![
        (
            "deepseek",
            "https://api.deepseek.com/anthropic",
            "deepseek-chat",
            "anthropic",
            &["DEEPSEEK_API_KEY"],
        ),
        (
            "kimi",
            "https://api.kimi.com/coding",
            "k3",
            "anthropic",
            &["KIMI_API_KEY"],
        ),
        // 月之暗面开放平台的老密钥与 kimi.com 不通用，单独保留一条 OpenAI 兼容泳道
        (
            "moonshot",
            "https://api.moonshot.cn/v1",
            "kimi-k2-0905-preview",
            "openai",
            &["MOONSHOT_API_KEY"],
        ),
        (
            "glm",
            "https://open.bigmodel.cn/api/anthropic",
            "glm-4.6",
            "anthropic",
            &["ZHIPU_API_KEY", "GLM_API_KEY"],
        ),
        (
            "qwen",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "qwen-plus",
            "openai",
            &["DASHSCOPE_API_KEY", "QWEN_API_KEY"],
        ),
        (
            "siliconflow",
            "https://api.siliconflow.cn/v1",
            "deepseek-ai/DeepSeek-V3",
            "openai",
            &["SILICONFLOW_API_KEY"],
        ),
        (
            "minimax",
            "https://api.minimaxi.com/anthropic",
            "MiniMax-M3",
            "anthropic",
            &["MINIMAX_API_KEY"],
        ),
    ]
}

/// 认 Claude Code 那套 ANTHROPIC_* 环境变量，让同一份 env 能直接驱动 i-agent。
/// 走 Anthropic 原生协议的额外好处：服务端已把推理内容剥离到独立 block，
/// content 不会混进 <think>，也就不会出现「过滤完变空 → 中止」。
fn anthropic_env_provider() -> Option<Provider> {
    let key = env_nonempty("ANTHROPIC_AUTH_TOKEN").or_else(|| env_nonempty("ANTHROPIC_API_KEY"))?;
    let base =
        env_nonempty("ANTHROPIC_BASE_URL").unwrap_or_else(|| "https://api.anthropic.com".into());
    let model = env_nonempty("ANTHROPIC_MODEL")
        .or_else(|| env_nonempty("ANTHROPIC_DEFAULT_HAIKU_MODEL"))
        .unwrap_or_else(|| "claude-haiku-4-5-20251001".into());
    Some(Provider {
        name: "anthropic".into(),
        protocol: detect_protocol("anthropic", &base),
        base,
        model,
        key,
    })
}

fn builtin_image_providers() -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    vec![
        (
            "zhipu",
            "https://open.bigmodel.cn/api/paas/v4",
            "cogview-4",
            "ZHIPU_API_KEY",
        ),
        (
            "siliconflow",
            "https://api.siliconflow.cn/v1",
            "Kwai-Kolors/Kolors",
            "SILICONFLOW_API_KEY",
        ),
        (
            "minimax",
            "https://api.minimaxi.com/v1",
            "image-01",
            "MINIMAX_API_KEY",
        ),
    ]
}

fn load_file(path: &Path) -> Option<ConfigFile> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

impl Config {
    pub fn load(
        workspace: Option<String>,
        provider_sel: Option<String>,
        model_override: Option<String>,
        quiet: bool,
        stateless: bool,
    ) -> Result<Config, String> {
        let workspace = match workspace {
            Some(w) => PathBuf::from(w),
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        };
        let workspace = workspace.canonicalize().unwrap_or(workspace);

        let mut cf = ConfigFile::default();
        for p in [
            home_dir().join(".i-agent").join("config.json"),
            workspace.join(".i-agent").join("config.json"),
        ] {
            if let Some(f) = load_file(&p) {
                if f.context_window.is_some() {
                    cf.context_window = f.context_window;
                }
                if f.max_output.is_some() {
                    cf.max_output = f.max_output;
                }
                if f.max_turns.is_some() {
                    cf.max_turns = f.max_turns;
                }
                if !f.providers.is_empty() {
                    cf.providers = f.providers;
                }
                if !f.image_providers.is_empty() {
                    cf.image_providers = f.image_providers;
                }
                if f.canvas_url.is_some() {
                    cf.canvas_url = f.canvas_url;
                }
                if f.canvas_id.is_some() {
                    cf.canvas_id = f.canvas_id;
                }
            }
        }

        // 显式配置的供应商：自定义环境变量 > ANTHROPIC_* > 配置文件。
        // 内置供应商（靠扫描 DEEPSEEK_API_KEY 等密钥入列）单独放一层：
        // 只要存在任何显式配置，内置层就不进自动候选/回退链 ——
        // 否则给生图工具准备的 MINIMAX_API_KEY 会悄悄把对话模型劫持到 MiniMax，
        // 无视用户显式设置的 ANTHROPIC_BASE_URL（跑错模型还一路显得很正常）。
        let mut explicit: Vec<Provider> = Vec::new();
        if let (Some(key), Some(base)) = (
            env_nonempty("I_AGENT_API_KEY"),
            env_nonempty("I_AGENT_BASE_URL"),
        ) {
            let proto =
                detect_protocol(&env_nonempty("I_AGENT_PROTOCOL").unwrap_or_default(), &base);
            explicit.push(Provider {
                name: "custom".into(),
                protocol: proto,
                base,
                model: env_nonempty("I_AGENT_MODEL").unwrap_or_else(|| "deepseek-chat".into()),
                key,
            });
        }
        if let Some(p) = anthropic_env_provider() {
            explicit.push(p);
        }
        for fp in &cf.providers {
            let key = resolve_key(&fp.key, &fp.key_env);
            explicit.push(Provider {
                name: fp.name.clone(),
                protocol: detect_protocol(&fp.protocol, &fp.base),
                base: fp.base.clone(),
                model: fp.model.clone(),
                key,
            });
        }
        let mut builtin: Vec<Provider> = Vec::new();
        for (name, base, model, protocol, key_envs) in builtin_providers() {
            if explicit.iter().any(|c| c.name == name) {
                continue;
            }
            let key = key_envs
                .iter()
                .find_map(|k| env_nonempty(k))
                .unwrap_or_default();
            builtin.push(Provider {
                name: name.into(),
                protocol: detect_protocol(protocol, base),
                base: base.into(),
                model: model.into(),
                key,
            });
        }

        let usable_explicit: Vec<Provider> = explicit
            .iter()
            .filter(|p| !p.key.is_empty())
            .cloned()
            .collect();
        let usable_builtin: Vec<Provider> = builtin
            .iter()
            .filter(|p| !p.key.is_empty())
            .cloned()
            .collect();

        // 自动选择：显式配置优先且排他；没有显式配置才轮到内置的密钥扫描
        let auto: Vec<Provider> = if !usable_explicit.is_empty() {
            usable_explicit.clone()
        } else {
            usable_builtin.clone()
        };

        let mut ordered: Vec<Provider> = match &provider_sel {
            Some(sel) => {
                // --provider 点名时，内置供应商仍可被选中（用户明说了就算数）
                let mut all = usable_explicit.clone();
                all.extend(usable_builtin.iter().cloned());
                let mut v: Vec<Provider> = all.iter().filter(|p| &p.name == sel).cloned().collect();
                if v.is_empty() {
                    return Err(format!(
                        "供应商 {sel} 不可用（未配置或缺密钥）。可用: {}",
                        all.iter()
                            .map(|p| p.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                v.extend(auto.iter().filter(|p| &p.name != sel).cloned());
                v
            }
            None => auto,
        };
        if ordered.is_empty() {
            return Err(
                "未找到任何可用的模型密钥。请设置以下环境变量之一:\n  ANTHROPIC_AUTH_TOKEN 或 ANTHROPIC_API_KEY (+ ANTHROPIC_BASE_URL / ANTHROPIC_MODEL)   Anthropic 协议端点\n  I_AGENT_API_KEY + I_AGENT_BASE_URL (+ I_AGENT_MODEL)   任意 OpenAI 兼容端点\n  KIMI_API_KEY (Kimi k3) / DEEPSEEK_API_KEY / ZHIPU_API_KEY (GLM) / MINIMAX_API_KEY\n  MOONSHOT_API_KEY / DASHSCOPE_API_KEY (通义) / SILICONFLOW_API_KEY"
                    .into(),
            );
        }
        let mut provider = ordered.remove(0);
        if let Some(m) = model_override {
            provider.model = m;
        }

        // 生图供应商
        let mut image_providers: Vec<ImageProvider> = Vec::new();
        for fp in &cf.image_providers {
            let key = resolve_key(&fp.key, &fp.key_env);
            if !key.is_empty() {
                image_providers.push(ImageProvider {
                    name: fp.name.clone(),
                    base: fp.base.clone(),
                    model: fp.model.clone(),
                    key,
                });
            }
        }
        for (name, base, model, key_env) in builtin_image_providers() {
            if image_providers.iter().any(|c| c.name == name) {
                continue;
            }
            if let Some(key) = env_nonempty(key_env) {
                image_providers.push(ImageProvider {
                    name: name.into(),
                    base: base.into(),
                    model: model.into(),
                    key,
                });
            }
        }

        let context_window = env_nonempty("I_AGENT_CTX")
            .and_then(|v| v.parse().ok())
            .or(cf.context_window)
            .unwrap_or(65536);
        let max_output = cf.max_output.unwrap_or(16384).min(context_window / 3);
        let canvas_url = env_nonempty("I_AGENT_CANVAS_URL")
            .or_else(|| env_nonempty("AI_CANVAS_API_URL"))
            .or(cf.canvas_url)
            .unwrap_or_else(|| "http://127.0.0.1:8787".into())
            .trim_end_matches('/')
            .to_string();
        let canvas_id = env_nonempty("I_AGENT_CANVAS_ID")
            .or_else(|| env_nonempty("AI_CANVAS_ID"))
            .or(cf.canvas_id)
            .unwrap_or_else(|| "main".into());

        Ok(Config {
            assets_dir: crate::skills::assets_dir(),
            workspace,
            provider,
            fallbacks: ordered,
            image_providers,
            context_window,
            max_output,
            max_turns: cf.max_turns.unwrap_or(96),
            quiet,
            stateless,
            canvas_url,
            canvas_id,
        })
    }

    /// 把（可能是相对的）路径解析到工作目录下
    pub fn resolve(&self, path: &str) -> PathBuf {
        let p = PathBuf::from(path);
        if p.is_absolute() {
            p
        } else {
            self.workspace.join(p)
        }
    }
}
