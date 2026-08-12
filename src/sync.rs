//! 本地 WebSocket 同步协议：消息解析与"我的帕鲁"去重合并（纯逻辑，不依赖 WASM）。
//!
//! 消息格式（mod 端推送）：
//! ```json
//! {"version":1,"source":"palws","pals":[
//!   {"species":"PinkCat","gender":"male","passives":["Brave"],"nickname":"...","level":12}
//! ]}
//! ```

use crate::planner::{Gender, OwnedPal, PalGroup};
use serde::Deserialize;

/// 协议版本，不匹配的消息整体忽略
pub const PROTOCOL_VERSION: u32 = 1;

/// 帕鲁数组；单只解析失败（如字段类型完全对不上）只跳过该只，不影响整条消息
#[derive(Debug, Deserialize)]
struct SyncMessage {
    version: u32,
    /// 来源标识（如 "palws"），仅记录不强制校验
    #[serde(default)]
    #[allow(dead_code)]
    source: String,
    #[serde(default, deserialize_with = "skip_bad_pals")]
    pals: Vec<SyncedPal>,
}

/// 逐只容错反序列化：解析失败的条目打 console 日志（WASM）后丢弃。
fn skip_bad_pals<'de, D>(de: D) -> Result<Vec<SyncedPal>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(de)?;
    Ok(raw
        .into_iter()
        .filter_map(|v| match serde_json::from_value::<SyncedPal>(v) {
            Ok(p) => Some(p),
            Err(_e) => {
                #[cfg(target_arch = "wasm32")]
                web_sys::console::warn_1(
                    &wasm_bindgen::JsValue::from_str(&format!("同步条目解析失败已跳过：{_e}")),
                );
                None
            }
        })
        .collect())
}

/// null 当空数组（mod 可能给无被动的帕鲁输出 `"passives": null`）。
fn null_as_empty<'de, D>(de: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Vec<String>>::deserialize(de)?.unwrap_or_default())
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

/// null 当空串。
fn null_as_empty_string<'de, D>(de: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(de)?.unwrap_or_default())
}

impl SyncedPal {
    /// 转成 OwnedPal 草稿（id 为 0，由 merge_owned 重新分配）；
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
    /// 协议版本不匹配（消息被忽略）
    UnsupportedVersion(u32),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Parse(e) => write!(f, "同步消息解析失败：{e}"),
            SyncError::UnsupportedVersion(v) => {
                write!(f, "同步协议版本不支持：{v}（期望 {PROTOCOL_VERSION}）")
            }
        }
    }
}

/// 解析一条同步消息；版本不匹配返回 UnsupportedVersion（调用方打日志后忽略）。
pub fn parse_message(text: &str) -> Result<Vec<SyncedPal>, SyncError> {
    let msg: SyncMessage =
        serde_json::from_str(text).map_err(|e| SyncError::Parse(e.to_string()))?;
    if msg.version != PROTOCOL_VERSION {
        return Err(SyncError::UnsupportedVersion(msg.version));
    }
    Ok(msg.pals)
}

/// 性别字段映射；"unknown" 及其他值返回 None（该只跳过——OwnedPal 没有未知性别）。
pub fn parse_gender(s: &str) -> Option<Gender> {
    match s {
        "male" => Some(Gender::Male),
        "female" => Some(Gender::Female),
        _ => None,
    }
}

/// 去重键：物种 + 性别 + 排序后的被动列表。
fn dedupe_key(p: &OwnedPal) -> (String, Gender, Vec<String>) {
    let mut passives = p.passives.clone();
    passives.sort();
    (p.species.clone(), p.gender, passives)
}

/// 把同步来的帕鲁合并进现有列表：
/// - 按 (species, gender, 排序后 passives) 去重，已存在（含 incoming 内部重复）的跳过；
/// - 新增条目 id 沿用现有惯例：当前最大 id + 1 递增（传入的 id 会被覆盖）。
///
/// 自动同步替换：游戏数据为唯一真相，整个列表重建为本次同步内容
/// （手动添加的帕鲁一并清除）。返回写入的数量。
pub fn overwrite_owned(existing: &mut Vec<OwnedPal>, incoming: Vec<OwnedPal>) -> usize {
    let n = incoming.len();
    let rebuilt: Vec<OwnedPal> = incoming
        .into_iter()
        .enumerate()
        .map(|(i, mut p)| {
            p.id = i as u64 + 1;
            p.synced = true;
            p
        })
        .collect();
    *existing = rebuilt;
    n
}

/// 返回实际新增的数量。
pub fn merge_owned(existing: &mut Vec<OwnedPal>, incoming: Vec<OwnedPal>) -> usize {
    let mut seen: std::collections::HashSet<_> = existing.iter().map(dedupe_key).collect();
    let mut next = existing.iter().map(|p| p.id).max().unwrap_or(0) + 1;
    let mut added = 0;
    for mut p in incoming {
        let key = dedupe_key(&p);
        if seen.insert(key.clone()) {
            p.id = next;
            next += 1;
            existing.push(p);
            added += 1;
        } else if let Some(old) = existing.iter_mut().find(|o| dedupe_key(o) == key) {
            // 同一只帕鲁再次同步：只刷新位置/头领/最爱标记（会变动）
            if old.group != p.group {
                old.group = p.group;
            }
            if old.is_boss != p.is_boss {
                old.is_boss = p.is_boss;
            }
            if old.favorite != p.favorite {
                old.favorite = p.favorite;
            }
            if old.is_lucky != p.is_lucky {
                old.is_lucky = p.is_lucky;
            }
            if old.nickname != p.nickname {
                old.nickname = p.nickname.clone();
            }
        }
    }
    added
}
