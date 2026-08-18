use crate::llm::Msg;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const MAIN: &str = "main";

/// 会话条目。会话不再是一条直线，而是一棵树：
/// 每条消息记住自己的父节点，分支就是「从某个父节点长出去的另一条链」。
/// 这样同一份前缀可以被 N 个变体分支共用 —— 前缀逐字节一致，模型侧才能命中前缀缓存。
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Entry {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
    pub branch: String,
    /// "msg" | "branch_summary"
    #[serde(default = "kind_msg")]
    pub kind: String,
    /// 分支的人类可读标签（变体名）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub msg: Msg,
}

fn kind_msg() -> String {
    "msg".into()
}

fn file(ws: &Path) -> PathBuf {
    ws.join(".i-agent").join("session.jsonl")
}

fn head_file(ws: &Path) -> PathBuf {
    ws.join(".i-agent").join("HEAD")
}

/// 全局写锁 + id 分配器。
/// 变体分支是并行跑的，多个线程同时往同一个 jsonl 追加：
/// id 分配和写盘必须在同一把锁里完成，否则会出现重复 id 或半行交错。
static WRITER: OnceLock<Mutex<u64>> = OnceLock::new();

fn writer() -> &'static Mutex<u64> {
    WRITER.get_or_init(|| Mutex::new(1))
}

/// 加载完会话后调用，把 id 分配器对齐到已有的最大 id + 1
pub fn init_ids(next: u64) {
    let mut g = writer().lock().unwrap_or_else(|e| e.into_inner());
    if next > *g {
        *g = next;
    }
}

/// 追加一条 entry，返回它的 id。线程安全。
pub fn append_entry(
    ws: &Path,
    branch: &str,
    parent: Option<u64>,
    kind: &str,
    label: Option<&str>,
    msg: &Msg,
) -> u64 {
    let mut g = writer().lock().unwrap_or_else(|e| e.into_inner());
    let id = *g;
    *g += 1;
    let e = Entry {
        id,
        parent,
        branch: branch.to_string(),
        kind: kind.to_string(),
        label: label.map(|s| s.to_string()),
        msg: msg.clone(),
    };
    let path = file(ws);
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    if let Ok(line) = serde_json::to_string(&e) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "{line}");
        }
    }
    id
}

/// 一条分支的写入游标：记住自己写到哪儿了，下一条挂在它后面。
#[derive(Clone, Debug)]
pub struct Sink {
    pub branch: String,
    pub head: Option<u64>,
    pub label: Option<String>,
}

impl Sink {
    pub fn new(branch: &str, head: Option<u64>, label: Option<String>) -> Sink {
        Sink { branch: branch.to_string(), head, label }
    }
    pub fn main(head: Option<u64>) -> Sink {
        Sink::new(MAIN, head, None)
    }
    pub fn append(&mut self, ws: &Path, msg: &Msg) {
        let id =
            append_entry(ws, &self.branch, self.head, "msg", self.label.as_deref(), msg);
        self.head = Some(id);
    }
    pub fn append_summary(&mut self, ws: &Path, msg: &Msg) {
        let id = append_entry(
            ws,
            &self.branch,
            self.head,
            "branch_summary",
            self.label.as_deref(),
            msg,
        );
        self.head = Some(id);
    }
}

#[derive(Debug)]
pub struct BranchInfo {
    pub name: String,
    pub label: Option<String>,
    pub head: u64,
    pub len: usize,
    pub fork_from: Option<u64>,
}

pub struct Log {
    pub entries: Vec<Entry>,
}

impl Log {
    /// 读取会话。向后兼容 v0.1 的裸 Msg 行：
    /// 老文件里每行就是一条 Msg，没有 id/parent/branch —— 按顺序补成 main 上的一条直链。
    pub fn load(ws: &Path) -> Log {
        let Ok(text) = std::fs::read_to_string(file(ws)) else {
            return Log { entries: Vec::new() };
        };
        let mut entries: Vec<Entry> = Vec::new();
        let mut legacy_seq: u64 = 0;
        let mut legacy_prev: Option<u64> = None;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(e) = serde_json::from_str::<Entry>(line) {
                entries.push(e);
                continue;
            }
            // 老格式回退：裸 Msg
            if let Ok(m) = serde_json::from_str::<Msg>(line) {
                legacy_seq += 1;
                // 老 id 从一个高位段起步，避免和新格式的 id 撞车
                let id = legacy_seq;
                entries.push(Entry {
                    id,
                    parent: legacy_prev,
                    branch: MAIN.into(),
                    kind: "msg".into(),
                    label: None,
                    msg: m,
                });
                legacy_prev = Some(id);
            }
        }
        let next = entries.iter().map(|e| e.id).max().unwrap_or(0) + 1;
        init_ids(next);
        Log { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn by_id(&self, id: u64) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// 某条分支的末端 entry id
    pub fn head(&self, branch: &str) -> Option<u64> {
        self.entries.iter().rev().find(|e| e.branch == branch).map(|e| e.id)
    }

    /// 从根到 id 的完整消息链（这就是要喂给模型的历史）
    pub fn chain(&self, id: u64) -> Vec<Msg> {
        let mut out: Vec<Msg> = Vec::new();
        let mut cur = Some(id);
        let mut guard = 0usize;
        while let Some(i) = cur {
            guard += 1;
            if guard > 100_000 {
                break; // 环保护：手改过的文件不该把进程转死
            }
            let Some(e) = self.by_id(i) else { break };
            out.push(e.msg.clone());
            cur = e.parent;
        }
        out.reverse();
        out
    }

    /// 从根到 id 的 entry 链（做分支摘要时要看 kind）
    pub fn chain_ids(&self, id: u64) -> Vec<u64> {
        let mut out: Vec<u64> = Vec::new();
        let mut cur = Some(id);
        let mut guard = 0usize;
        while let Some(i) = cur {
            guard += 1;
            if guard > 100_000 {
                break;
            }
            let Some(e) = self.by_id(i) else { break };
            out.push(e.id);
            cur = e.parent;
        }
        out.reverse();
        out
    }

    /// 两条分支的最近公共祖先
    pub fn common_ancestor(&self, a: u64, b: u64) -> Option<u64> {
        let ca = self.chain_ids(a);
        let cb: std::collections::HashSet<u64> = self.chain_ids(b).into_iter().collect();
        ca.into_iter().rev().find(|i| cb.contains(i))
    }

    /// 从 `from`（不含）走到 `leaf`（含）之间的消息 —— 即「被放弃的那段工作」
    pub fn since(&self, from: Option<u64>, leaf: u64) -> Vec<Msg> {
        let chain = self.chain_ids(leaf);
        let start = match from {
            Some(f) => chain.iter().position(|i| *i == f).map(|p| p + 1).unwrap_or(0),
            None => 0,
        };
        chain[start..].iter().filter_map(|i| self.by_id(*i)).map(|e| e.msg.clone()).collect()
    }

    pub fn branches(&self) -> Vec<BranchInfo> {
        let mut names: Vec<String> = Vec::new();
        for e in &self.entries {
            if !names.iter().any(|n| n == &e.branch) {
                names.push(e.branch.clone());
            }
        }
        names
            .into_iter()
            .filter_map(|name| {
                let head = self.head(&name)?;
                let len = self.entries.iter().filter(|e| e.branch == name).count();
                let label = self
                    .entries
                    .iter()
                    .find(|e| e.branch == name && e.label.is_some())
                    .and_then(|e| e.label.clone());
                // 派生点 = 本分支最早那条 entry 的 parent
                let fork_from = self
                    .entries
                    .iter()
                    .find(|e| e.branch == name)
                    .and_then(|e| e.parent);
                Some(BranchInfo { name, label, head, len, fork_from })
            })
            .collect()
    }
}

/// 当前活跃分支指针
pub fn read_head(ws: &Path) -> String {
    std::fs::read_to_string(head_file(ws))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| MAIN.to_string())
}

pub fn write_head(ws: &Path, branch: &str) {
    let path = head_file(ws);
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let _ = std::fs::write(path, branch);
}

pub fn clear(ws: &Path) {
    let _ = std::fs::remove_file(file(ws));
    let _ = std::fs::remove_file(head_file(ws));
    let mut g = writer().lock().unwrap_or_else(|e| e.into_inner());
    *g = 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "iagent-sess-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(d.join(".i-agent"));
        d
    }

    #[test]
    fn legacy_flat_session_loads_as_main_chain() {
        let ws = tmp();
        let p = file(&ws);
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        std::fs::write(
            &p,
            "{\"role\":\"user\",\"content\":\"a\"}\n{\"role\":\"assistant\",\"content\":\"b\"}\n",
        )
        .unwrap();
        let log = Log::load(&ws);
        assert_eq!(log.entries.len(), 2);
        assert!(log.entries.iter().all(|e| e.branch == MAIN));
        assert_eq!(log.entries[1].parent, Some(log.entries[0].id));
        let head = log.head(MAIN).unwrap();
        let msgs = log.chain(head);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content.as_deref(), Some("a"));
        assert_eq!(msgs[1].content.as_deref(), Some("b"));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn branches_share_prefix_and_diverge() {
        let ws = tmp();
        clear(&ws);
        let mut main = Sink::main(None);
        main.append(&ws, &Msg::text("system", "S"));
        main.append(&ws, &Msg::text("user", "共享任务"));
        let fork = main.head;

        let mut b1 = Sink::new("v1", fork, Some("小红书".into()));
        b1.append(&ws, &Msg::text("user", "风格A"));
        let mut b2 = Sink::new("v2", fork, Some("公众号".into()));
        b2.append(&ws, &Msg::text("user", "风格B"));

        let log = Log::load(&ws);
        let c1 = log.chain(log.head("v1").unwrap());
        let c2 = log.chain(log.head("v2").unwrap());
        // 前缀逐字节一致 —— 这是前缀缓存能命中的前提
        assert_eq!(c1.len(), 3);
        assert_eq!(c2.len(), 3);
        assert_eq!(c1[0].content, c2[0].content);
        assert_eq!(c1[1].content, c2[1].content);
        assert_ne!(c1[2].content, c2[2].content);
        assert_eq!(log.common_ancestor(log.head("v1").unwrap(), log.head("v2").unwrap()), fork);
        assert_eq!(log.branches().len(), 3);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn since_returns_only_abandoned_tail() {
        let ws = tmp();
        clear(&ws);
        let mut main = Sink::main(None);
        main.append(&ws, &Msg::text("system", "S"));
        let fork = main.head;
        main.append(&ws, &Msg::text("user", "丢弃1"));
        main.append(&ws, &Msg::text("assistant", "丢弃2"));
        let log = Log::load(&ws);
        let tail = log.since(fork, log.head(MAIN).unwrap());
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].content.as_deref(), Some("丢弃1"));
        let _ = std::fs::remove_dir_all(&ws);
    }
}
