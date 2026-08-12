//! WebSocket 同步客户端与侧栏 UI：连接本地 mod 服务，收到帕鲁列表后由用户确认合并。
//!
//! 纯协议/合并逻辑在 `sync` 模块；本文件只做 WASM 侧的连接管理与展示。

use crate::planner::OwnedPal;
use crate::sync::{self, SyncedPal};
use crate::ui::{BtnVariant, Button};
use crate::{OwnedStore, db, passive_by_internal};
use dioxus::prelude::*;
use futures_util::StreamExt;
use gloo_net::websocket::{Message, futures::WebSocket};
use gloo_timers::future::TimeoutFuture;

/// 本地 mod 的 WebSocket 地址
const WS_URL: &str = "ws://127.0.0.1:32123/ws";
/// 重连退避：1s → 2s → … → 上限 30s
const RECONNECT_MIN_MS: u32 = 1_000;
const RECONNECT_MAX_MS: u32 = 30_000;

/// 连接状态（侧栏底部小字展示）。
/// Synced 是粘滞状态：收到过一次同步后保持，不再随断线回退。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    Disconnected,
    Connected,
    Synced,
}

impl SyncStatus {
    fn label(self) -> &'static str {
        match self {
            SyncStatus::Disconnected => "未连接",
            SyncStatus::Connected => "已连接",
            SyncStatus::Synced => "已收到同步",
        }
    }
}

/// 同步状态的全局 store：连接状态 + 待确认的帕鲁列表 + 自动同步开关 + 结果 toast。
#[derive(Debug, Clone, Copy)]
pub struct SyncStore {
    pub status: Signal<SyncStatus>,
    pub pending: Signal<Vec<SyncedPal>>,
    /// 自动同步开关（开启后收到同步直接入库并弹 toast）
    pub auto_sync: Signal<bool>,
    /// 合并结果 toast（显示几秒后自动消失）
    pub toast: Signal<Option<String>>,
}

/// 把一批同步帕鲁合并进持有列表：过滤（物种/性别/图鉴/被动）+ 去重 + 详细 console 日志。
/// 返回汇总文本（用于 toast / 结果条）。
pub fn do_merge(pals: &mut Vec<OwnedPal>, synced: Vec<SyncedPal>, overwrite: bool) -> String {
    let received = synced.len();
    let incoming: Vec<OwnedPal> = synced
        .iter()
        .filter_map(|sp| {
            let species = sp.species.clone().unwrap_or_else(|| "<null>".into());
            let Some(mut pal) = sp.to_owned_draft() else {
                web_sys::console::log_1(
                    &format!("[sync] 跳过 {species}（gender={}）：species 缺失或性别 unknown", sp.gender).into(),
                );
                return None;
            };
            if db().pal(&pal.species).is_none() {
                // 逐级归一：头目变体剥 BOSS_ 前缀 → 大小写不敏感匹配图鉴规范名
                let base = sync::strip_boss_prefix(&pal.species);
                let canonical = if db().pal(base).is_some() {
                    Some(base.to_string())
                } else {
                    db().canonical_name_ci(base).map(|s| s.to_string())
                };
                match canonical {
                    Some(c) => {
                        web_sys::console::log_1(
                            &format!("[sync] {} → 按 {} 入册", pal.species, c).into(),
                        );
                        pal.species = c;
                    }
                    None => {
                        web_sys::console::log_1(
                            &format!("[sync] 跳过 {}：图鉴无此物种", pal.species).into(),
                        );
                        return None;
                    }
                }
            }
            let before = pal.passives.len();
            let unknown: Vec<String> = pal
                .passives
                .iter()
                .filter(|ps| passive_by_internal(ps).is_none())
                .cloned()
                .collect();
            pal.passives.retain(|ps| passive_by_internal(ps).is_some());
            if !unknown.is_empty() {
                web_sys::console::log_1(
                    &format!("[sync] {}：剔除 {} 个未知被动 {:?}", pal.species, before - pal.passives.len(), unknown).into(),
                );
            }
            pal.passives.truncate(4);
            Some(pal)
        })
        .collect();
    let unrecognized = received - incoming.len();
    let added = if overwrite {
        sync::overwrite_owned(pals, incoming)
    } else {
        sync::merge_owned(pals, incoming)
    };
    let dupes = received - unrecognized - added;
    web_sys::console::log_1(
        &format!("[sync] {}：收到 {received}，写入 {added}，重复 {dupes}，无法识别 {unrecognized}",
            if overwrite { "覆盖" } else { "合并" }).into(),
    );

    let mut msg = if overwrite {
        format!("已同步替换 {added} 只")
    } else {
        format!("已导入 {added} 只")
    };
    let mut skipped = Vec::new();
    if dupes > 0 {
        skipped.push(format!("{dupes} 只重复"));
    }
    if unrecognized > 0 {
        skipped.push(format!("{unrecognized} 只无法识别"));
    }
    if !skipped.is_empty() {
        msg.push_str(&format!("（{}已跳过）", skipped.join("、")));
    }
    msg
}

fn console_warn(msg: &str) {
    web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(msg));
}

/// 启动 WebSocket 客户端（App 挂载时调用一次）。
/// 游戏/mod 未启动时连接失败属正常，静默按指数退避重试，不打扰用户。
pub fn use_ws_sync() {
    let sync = use_context::<SyncStore>();
    let store = use_context::<OwnedStore>();
    let mut owned_pals = store.pals;
    let mut status = sync.status;
    let mut pending = sync.pending;
    let mut toast = sync.toast;
    use_hook(move || {
        spawn(async move {
            let mut delay_ms = RECONNECT_MIN_MS;
            loop {
                match WebSocket::open(WS_URL) {
                    Ok(mut ws) => {
                        // 连上即重置退避；Synced 粘滞，不回落为 Connected
                        delay_ms = RECONNECT_MIN_MS;
                        if *status.peek() != SyncStatus::Synced {
                            status.set(SyncStatus::Connected);
                        }
                        while let Some(msg) = ws.next().await {
                            match msg {
                                Ok(Message::Text(text)) => match sync::parse_message(&text) {
                                    Ok(pals) => {
                                        if *sync.auto_sync.peek() {
                                            // 自动同步覆盖：游戏数据为权威，直接入库（覆盖旧同步数据）
                                            let summary =
                                                do_merge(&mut owned_pals.write(), pals, true);
                                            toast.set(Some(summary));
                                        } else {
                                            pending.set(pals);
                                        }
                                        status.set(SyncStatus::Synced);
                                    }
                                    Err(e) => console_warn(&e.to_string()),
                                },
                                // 二进制消息忽略
                                Ok(_) => {}
                                // 出错或对方关闭 → 退出读循环走重连
                                Err(_) => break,
                            }
                        }
                    }
                    Err(_) => {}
                }
                if *status.peek() != SyncStatus::Synced {
                    status.set(SyncStatus::Disconnected);
                }
                TimeoutFuture::new(delay_ms).await;
                delay_ms = (delay_ms * 2).min(RECONNECT_MAX_MS);
            }
        });
    });
}

/// 侧栏提示条：有待确认列表时展示「从游戏同步：N 只帕鲁」，确认后才合并。
#[component]
pub fn SyncBanner() -> Element {
    let store = use_context::<OwnedStore>();
    let sync = use_context::<SyncStore>();
    let mut pals = store.pals;
    let mut pending = sync.pending;
    // 合并/忽略后的结果提示（如「已导入 3 只」）
    let mut result = use_signal(|| None::<String>);

    let pending_count = pending.read().len();

    let merge = move |_| {
        let list = std::mem::take(&mut *pending.write());
        result.set(Some(do_merge(&mut pals.write(), list, false)));
    };

    rsx! {
        if pending_count > 0 {
            div { class: "sync-banner",
                span { class: "sync-banner-text", "从游戏同步：{pending_count} 只帕鲁" }
                Button { sm: true, onclick: merge, "合并" }
                Button {
                    variant: BtnVariant::Ghost,
                    sm: true,
                    onclick: move |_| {
                        pending.set(Vec::new());
                        result.set(Some("已忽略本次同步".to_string()));
                    },
                    "忽略"
                }
            }
        } else if let Some(msg) = result.read().clone() {
            div { class: "sync-banner sync-banner--done",
                span { class: "sync-banner-text", "{msg}" }
                Button {
                    variant: BtnVariant::Ghost,
                    sm: true,
                    icon: true,
                    onclick: move |_| result.set(None),
                    "✕"
                }
            }
        }
    }
}

/// 页脚状态栏左侧：连接状态 + 自动同步开关。
#[component]
pub fn SyncStatusBar() -> Element {
    let sync = use_context::<SyncStore>();
    let status = *sync.status.read();
    let mut auto = sync.auto_sync;
    let dot_class = match status {
        SyncStatus::Disconnected => "off",
        SyncStatus::Connected => "on",
        SyncStatus::Synced => "synced",
    };
    rsx! {
        div { class: "sync-status",
            span { class: "sync-status-dot sync-status-dot--{dot_class}" }
            "游戏同步：{status.label()}"
            label { class: "sync-switch",
                input {
                    r#type: "checkbox",
                    checked: auto,
                    onchange: move |e| auto.set(e.checked()),
                }
                span { class: "sync-switch-track" }
                "自动同步"
            }
        }
    }
}

/// 合并结果 toast（右下角，几秒后自动消失）。
#[component]
pub fn SyncToast() -> Element {
    let sync = use_context::<SyncStore>();
    let mut toast = sync.toast;
    // toast 变化时重置 4 秒自动消失计时
    use_effect(move || {
        if toast.read().is_some() {
            spawn(async move {
                TimeoutFuture::new(4000).await;
                toast.set(None);
            });
        }
    });
    rsx! {
        if let Some(msg) = toast.read().as_ref() {
            div { class: "sync-toast", "{msg}" }
        }
    }
}
