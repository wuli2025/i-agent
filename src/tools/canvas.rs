//! AI Canvas 控制面：把 i-agent 工具调用转发到画布的 Agent-first API。
use crate::config::Config;
use serde_json::{Map, Value};
use std::time::Duration;

const CANVAS_SPEC: &str = r#"{
  "type": "function",
  "function": {
    "name": "canvas",
    "description": "操作 Polaris AI 无限画布。先 query 查节点 ID；add/update/remove/connect 管理节点；arrange 排列卡片；storyboard 创建 9/25 宫格分镜；run 后用 wait 等完成。UI 与本工具共用 CommandBus。",
    "parameters": {
      "type": "object",
      "required": ["action"],
      "properties": {
        "action": {"type":"string","enum":["health","query","add","update","remove","connect","arrange","storyboard","group","run","wait","commands"]},
        "canvasId": {"type":"string","description":"默认 main"},
        "detail": {"type":"string","enum":["summary","full"]},
        "type": {"type":"string","description":"text/image/video/audio/script/storyboard.shot/gen.t2i/gen.i2v"},
        "nodeId": {"type":"string"},
        "nodeIds": {"type":"array","items":{"type":"string"}},
        "title": {"type":"string"},
        "x": {"type":"number"},
        "y": {"type":"number"},
        "width": {"type":"number"},
        "height": {"type":"number"},
        "params": {"type":"object","description":"节点参数；update 时按 key 合并"},
        "source": {"type":"string"},
        "sourceHandle": {"type":"string"},
        "target": {"type":"string"},
        "targetHandle": {"type":"string"},
        "dataType": {"type":"string","enum":["image","video","audio","text","mask","script","any"]},
        "layout": {"type":"string","enum":["storyboard","grid","horizontal","vertical"]},
        "columns": {"type":"integer","minimum":1,"maximum":25},
        "gapX": {"type":"number"},
        "gapY": {"type":"number"},
        "origin": {"type":"object","required":["x","y"],"properties":{"x":{"type":"number"},"y":{"type":"number"}}},
        "shots": {"type":"array","maxItems":100,"items":{"type":"object","required":["title"],"properties":{"title":{"type":"string"},"prompt":{"type":"string"},"notes":{"type":"string"},"duration":{"type":"number"},"shotSize":{"type":"string"},"cameraMovement":{"type":"string"},"imageUrl":{"type":"string"},"character":{"type":"string"},"dialogue":{"type":"string"}}}},
        "connectSequentially": {"type":"boolean"},
        "color": {"type":"string"},
        "targetIds": {"type":"array","items":{"type":"string"}},
        "promptId": {"type":"string"},
        "timeoutMs": {"type":"integer","maximum":300000},
        "commands": {"type":"array","items":{"type":"object"}}
      }
    }
  }
}"#;

pub fn spec() -> Value {
    serde_json::from_str(CANVAS_SPEC).expect("内置 canvas 工具 schema 必须是合法 JSON")
}

fn response_text(response: ureq::Response) -> String {
    response
        .into_string()
        .unwrap_or_else(|e| format!("读取响应失败: {e}"))
}

fn request_json(method: &str, url: &str, body: Option<&Value>) -> Result<String, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(310))
        .timeout_write(Duration::from_secs(30))
        .build();
    let result = match method {
        "GET" => agent.get(url).call(),
        "POST" => agent
            .post(url)
            .set("content-type", "application/json")
            .send_string(&body.unwrap_or(&Value::Null).to_string()),
        _ => return Err(format!("不支持的 HTTP 方法 {method}")),
    };
    match result {
        Ok(response) => Ok(response_text(response)),
        Err(ureq::Error::Status(code, response)) => {
            let text = response_text(response);
            Err(format!("AI Canvas API 返回 HTTP {code}: {text}"))
        }
        Err(error) => Err(format!(
            "无法连接 AI Canvas API {url}: {error}。请先在 ai-canvas 仓库运行 npm run dev"
        )),
    }
}

fn valid_canvas_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 {
        return false;
    }
    let mut characters = id.chars();
    characters.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && characters.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn run(args: &Value, cfg: &Config) -> Result<String, String> {
    let object = args.as_object().ok_or("canvas 参数必须是 JSON 对象")?;
    let action = object
        .get("action")
        .and_then(Value::as_str)
        .ok_or("canvas 缺少 action")?;
    if action == "health" {
        return request_json("GET", &format!("{}/health", cfg.canvas_url), None);
    }

    let canvas_id = object
        .get("canvasId")
        .or_else(|| object.get("canvas_id"))
        .and_then(Value::as_str)
        .unwrap_or(&cfg.canvas_id);
    if !valid_canvas_id(canvas_id) {
        return Err("canvasId 只能包含字母、数字、下划线和连字符，最长 128 字符".into());
    }

    let api_action = match action {
        "query" => "query",
        "add" | "add_node" => "add_node",
        "update" | "update_node" => "update_node",
        "remove" | "remove_nodes" => "remove_nodes",
        "connect" => "connect",
        "arrange" => "arrange",
        "storyboard" | "create_storyboard" => "storyboard",
        "group" => "group",
        "run" => "run",
        "wait" => "wait",
        "commands" | "apply_commands" => "commands",
        other => return Err(format!("未知 canvas action: {other}")),
    };

    let mut body = Map::new();
    for (key, value) in object {
        if key != "canvasId" && key != "canvas_id" {
            body.insert(key.clone(), value.clone());
        }
    }
    body.insert("action".into(), Value::String(api_action.into()));
    if api_action == "query" && !body.contains_key("detail") {
        body.insert("detail".into(), Value::String("summary".into()));
    }
    if api_action == "arrange" {
        body.entry("layout")
            .or_insert_with(|| Value::String("storyboard".into()));
        body.entry("columns")
            .or_insert_with(|| Value::Number(3.into()));
    }

    let url = format!("{}/api/canvases/{}/agent", cfg.canvas_url, canvas_id);
    request_json("POST", &url, Some(&Value::Object(body)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{detect_protocol, Provider};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread;

    fn test_config(canvas_url: String) -> Config {
        Config {
            workspace: PathBuf::from("."),
            provider: Provider {
                name: "test".into(),
                base: String::new(),
                model: String::new(),
                key: String::new(),
                protocol: detect_protocol("", ""),
            },
            fallbacks: vec![],
            image_providers: vec![],
            context_window: 32768,
            max_output: 4096,
            max_turns: 8,
            assets_dir: PathBuf::from("."),
            quiet: true,
            stateless: false,
            canvas_url,
            canvas_id: "main".into(),
        }
    }

    fn read_request(stream: &mut std::net::TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 2048];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&request);
            let Some(header_end) = text.find("\r\n\r\n") else {
                continue;
            };
            let content_length = text[..header_end]
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }
        String::from_utf8(request).unwrap()
    }

    #[test]
    fn canvas_ids_are_strict() {
        assert!(valid_canvas_id("main_2026-demo"));
        assert!(!valid_canvas_id("../main"));
        assert!(!valid_canvas_id("-leading-dash"));
        assert!(!valid_canvas_id("_leading_underscore"));
        assert!(!valid_canvas_id("含中文"));
    }

    #[test]
    fn forwards_storyboard_alias_and_defaults_to_agent_api() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            let response = r#"{"ok":true,"nodeIds":["node-1"]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                response.len(),
                response
            )
            .unwrap();
            request
        });

        let cfg = test_config(format!("http://{address}"));
        let result = run(
            &serde_json::json!({
                "action": "create_storyboard",
                "canvasId": "agent-board",
                "title": "九宫格",
                "shots": [{"title": "镜头 01"}]
            }),
            &cfg,
        )
        .unwrap();
        assert!(result.contains("node-1"));

        let request = server.join().unwrap();
        assert!(request.starts_with("POST /api/canvases/agent-board/agent HTTP/1.1"));
        let body = request.split_once("\r\n\r\n").unwrap().1;
        let payload: Value = serde_json::from_str(body).unwrap();
        assert_eq!(payload["action"], "storyboard");
        assert_eq!(payload["title"], "九宫格");
        assert!(payload.get("canvasId").is_none());
    }
}
