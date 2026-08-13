//! 本地 WebSocket 同步协议：typed envelope 解析与"我的帕鲁"合并（纯逻辑，不依赖 WASM）。
//!
//! 服务端（mod）推送的每条消息都符合统一 envelope：
//! ```json
//! {"protocol":"palws","version":1,"type":"snapshot","id":"...","request_id":"...",
//!  "seq":42,"timestamp_ms":1786595000000,"payload":{...}}
//! ```
//!
//! v1 只支持 `snapshot` 的 `mode: "replace"`：网页收到后按"全量替换"重建
//! 同步列表（`synced=true` 的旧条目全部替换，`synced=false` 的手工条目保留）。

use crate::planner::{Gender, OwnedPal, PalGroup};
use serde::Deserialize;

/// 协议名与主版本，不匹配的消息整体忽略。
pub const PROTOCOL_NAME: &str = "palws";
pub const PROTOCOL_VERSION: u32 = 1;

/// 一条服务端事件（envelope 头已解析、校验，payload 按类型分发）。
#[derive(Debug, Clone, PartialEq)]
pub enum ServerEvent {
    Hello {
        server_version: String,
        capabilities: Vec<String>,
        clients: usize,
        sync_state: String,
    },
    SyncStatus {
        request_id: Option<String>,
        phase: String,
        requested_pages: u32,
        total_pages: u32,
        trigger: String,
    },
    Snapshot {
        request_id: Option<String>,
        seq: u64,
        mode: String,
        pals: Vec<SyncedPal>,
        stats: SnapshotStats,
    },
    Log {
        request_id: Option<String>,
        level: String,
        source: String,
        message: String,
    },
    Error {
        request_id: Option<String>,
        code: String,
        message: String,
        retryable: bool,
    },
    Pong {
        echo_id: String,
    },
    Unknown {
        mtype: String,
    },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SnapshotStats {
    pub total: u32,
    pub requested_pages: u32,
    pub request_errors: u32,
    pub containers: u32,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    version: Option<u32>,
    #[serde(rename = "type", default)]
    mtype: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    seq: Option<u64>,
    #[serde(default)]
    payload: Option<serde_json::Value>,
}

/// mod 推送的一只帕鲁。`species`/`passives` 为 internal_name；`nickname`/`level` 目前仅透传不使用。
/// 所有字段容错：缺省或 null 都按 None / 空处理，坏条目在上一层被丢弃。
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct SyncedPal {
    /// 物种 internal_name；缺省或 null → None（该只跳过）
    #[serde(default)]
    pub species: Option<String>,
    /// "male" / "female" / "unknown"；缺省或 null → 视为 unknown
    #[serde(default, deserialize_with = "null_as_empty_string")]
    pub gender: String,
    /// 被动 internal_name 列表；null → 空数组。垃圾名字（如 "RemoteUnrealParam: ..."）
    /// 在此原样保留，由合并方按内置被动表剔除。
    #[serde(default, deserialize_with = "null_as_empty")]
    pub passives: Vec<String>,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub level: Option<u32>,
    /// 最爱标记：0=无，1/2/3 对应游戏内 I/II/III（mod 内存直读）；缺省 0
    #[serde(default)]
    pub favorite: u8,
    /// 幸运（闪光）标记，对应游戏 IsRarePal；缺省 false
    #[serde(default)]
    pub lucky: bool,
    /// 据点序号（仅 base 帕鲁有）；缺省 → None
    #[serde(default)]
    pub basecamp: Option<u8>,
    /// 容器分组："party" / "box" / "base"（mod 按容器大小打标）；缺省 → 盒子
    #[serde(default)]
    pub group: Option<String>,
}

/// null 当空数组。
fn null_as_empty<'de, D>(de: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Vec<String>>::deserialize(de)?.unwrap_or_default())
}

/// null 当空串。
fn null_as_empty_string<'de, D>(de: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(de)?.unwrap_or_default())
}

impl SyncedPal {
    /// 转成 OwnedPal 草稿（id 为 0，由合并方重新分配）；
    /// 物种缺失或性别 unknown 返回 None（该只跳过——OwnedPal 没有未知性别）。
    /// 注意：不做物种/被动有效性校验，那是合并方（sync_client）的职责。
    pub fn to_owned_draft(&self) -> Option<OwnedPal> {
        let species = self.species.clone()?;
        Some(OwnedPal {
            id: 0,
            is_boss: strip_boss_prefix(&species) != species,
            species,
            gender: parse_gender(&self.gender)?,
            passives: self.passives.clone(),
            group: parse_group(self.group.as_deref()),
            favorite: self.favorite,
            nickname: self.nickname.clone(),
            is_lucky: self.lucky,
            basecamp: self.basecamp,
            synced: true,
        })
    }
}

/// "party"/"box"/"base" → PalGroup；未知或缺省 → 盒子
pub fn parse_group(g: Option<&str>) -> PalGroup {
    match g {
        Some("party") => PalGroup::Party,
        Some("base") => PalGroup::Base,
        _ => PalGroup::Box,
    }
}

/// 头目（阿尔法）帕鲁的物种名前缀变体（BOSS_/Boss_）；配种按原种算。
/// 返回剥掉前缀后的原种名，无前缀则原样返回。
pub fn strip_boss_prefix(species: &str) -> &str {
    const PREFIXES: [&str; 3] = ["BOSS_", "Boss_", "boss_"];
    for p in PREFIXES {
        if let Some(rest) = species.strip_prefix(p) {
            if !rest.is_empty() {
                return rest;
            }
        }
    }
    species
}

#[derive(Debug, Clone, PartialEq)]
pub enum SyncError {
    /// JSON 解析失败
    Parse(String),
    /// 协议名不匹配
    UnsupportedProtocol(String),
    /// 协议版本不匹配
    UnsupportedVersion(u32),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Parse(e) => write!(f, "同步消息解析失败：{e}"),
            SyncError::UnsupportedProtocol(p) => write!(f, "同步协议不支持：{p}"),
            SyncError::UnsupportedVersion(v) => {
                write!(f, "同步协议版本不支持：{v}（期望 {PROTOCOL_VERSION}）")
            }
        }
    }
}

fn parse_pals(payload: &serde_json::Value) -> Vec<SyncedPal> {
    let Some(arr) = payload.get("pals").and_then(|p| p.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| match serde_json::from_value::<SyncedPal>(v.clone()) {
            Ok(p) => Some(p),
            Err(_e) => {
                #[cfg(target_arch = "wasm32")]
                web_sys::console::warn_1(
                    &wasm_bindgen::JsValue::from_str(&format!("同步条目解析失败已跳过：{_e}")),
                );
                None
            }
        })
        .collect()
}

fn parse_stats(payload: &serde_json::Value) -> SnapshotStats {
    let s = payload.get("stats").and_then(|s| s.as_object());
    SnapshotStats {
        total: s.and_then(|s| s.get("total")).and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        requested_pages: s
            .and_then(|s| s.get("requested_pages"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        request_errors: s
            .and_then(|s| s.get("request_errors"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        containers: s
            .and_then(|s| s.get("containers"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
    }
}

/// 解析一条服务端消息；协议/版本不匹配返回 SyncError（调用方打日志后忽略）。
pub fn parse_server_message(text: &str) -> Result<ServerEvent, SyncError> {
    let env: Envelope =
        serde_json::from_str(text).map_err(|e| SyncError::Parse(e.to_string()))?;
    if env.protocol.as_deref() != Some(PROTOCOL_NAME) {
        return Err(SyncError::UnsupportedProtocol(
            env.protocol.unwrap_or_default(),
        ));
    }
    if let Some(v) = env.version {
        if v != PROTOCOL_VERSION {
            return Err(SyncError::UnsupportedVersion(v));
        }
    }
    let payload = env.payload.unwrap_or(serde_json::Value::Null);
    let ev = match env.mtype.as_str() {
        "server.hello" => ServerEvent::Hello {
            server_version: payload
                .get("server_version")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            capabilities: payload
                .get("capabilities")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            clients: payload.get("clients").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
            sync_state: payload
                .get("sync_state")
                .and_then(|v| v.as_str())
                .unwrap_or("idle")
                .to_string(),
        },
        "sync.status" => ServerEvent::SyncStatus {
            request_id: env.request_id.clone(),
            phase: payload
                .get("phase")
                .and_then(|v| v.as_str())
                .unwrap_or("idle")
                .to_string(),
            requested_pages: payload
                .get("requested_pages")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            total_pages: payload
                .get("total_pages")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            trigger: payload
                .get("trigger")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        "snapshot" => ServerEvent::Snapshot {
            request_id: env.request_id.clone(),
            seq: env.seq.unwrap_or(0),
            mode: payload
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("replace")
                .to_string(),
            pals: parse_pals(&payload),
            stats: parse_stats(&payload),
        },
        "log" => ServerEvent::Log {
            request_id: env.request_id.clone(),
            level: payload
                .get("level")
                .and_then(|v| v.as_str())
                .unwrap_or("info")
                .to_string(),
            source: payload
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("lua")
                .to_string(),
            message: payload
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        "error" => ServerEvent::Error {
            request_id: env.request_id.clone(),
            code: payload
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("error")
                .to_string(),
            message: payload
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            retryable: payload.get("retryable").and_then(|v| v.as_bool()).unwrap_or(false),
        },
        "pong" => ServerEvent::Pong {
            echo_id: payload
                .get("echo_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        other => ServerEvent::Unknown {
            mtype: other.to_string(),
        },
    };
    Ok(ev)
}

/// 性别字段映射；"unknown" 及其他值返回 None（该只跳过——OwnedPal 没有未知性别）。
pub fn parse_gender(s: &str) -> Option<Gender> {
    match s {
        "male" => Some(Gender::Male),
        "female" => Some(Gender::Female),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// client -> server 请求构造
// ---------------------------------------------------------------------------

pub fn client_hello_json(client_version: &str) -> String {
    serde_json::json!({
        "protocol": PROTOCOL_NAME,
        "version": PROTOCOL_VERSION,
        "type": "client.hello",
        "id": "hello-1",
        "payload": {"client": "pal-companion", "client_version": client_version},
    })
    .to_string()
}

pub fn snapshot_request_json(id: &str) -> String {
    serde_json::json!({
        "protocol": PROTOCOL_NAME,
        "version": PROTOCOL_VERSION,
        "type": "snapshot.request",
        "id": id,
        "payload": {},
    })
    .to_string()
}

pub fn ping_json(id: &str) -> String {
    serde_json::json!({
        "protocol": PROTOCOL_NAME,
        "version": PROTOCOL_VERSION,
        "type": "ping",
        "id": id,
        "payload": {},
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// 合并语义
// ---------------------------------------------------------------------------

/// v1 推荐语义：全量替换"同步条目"，保留"手工条目"。
/// `synced=true` 的旧条目全部移除；`synced=false` 的手工条目保留，incoming 追加其后。
/// 返回写入（incoming）的数量。
pub fn replace_synced(existing: &mut Vec<OwnedPal>, incoming: Vec<OwnedPal>) -> usize {
    let mut manual: Vec<OwnedPal> = existing
        .iter()
        .filter(|p| !p.synced)
        .cloned()
        .collect();
    let mut next = manual.iter().map(|p| p.id).max().unwrap_or(0) + 1;
    let n = incoming.len();
    for mut p in incoming {
        p.id = next;
        next += 1;
        p.synced = true;
        manual.push(p);
    }
    *existing = manual;
    n
}
