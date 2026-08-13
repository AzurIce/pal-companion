//! WebSocket 同步客户端：连接本地 mod 服务，解析 typed envelope，
//! 收到 snapshot 后按 v1 语义全量替换同步列表（保留手工条目）。
//!
//! 纯协议/合并逻辑在 `sync` 模块；本文件只做 WASM 侧的连接管理、状态机与展示。

use crate::planner::OwnedPal;
use crate::sync::{self, ServerEvent, SyncedPal};
use crate::ui::{BtnVariant, Button};
use crate::{OwnedStore, db, passive_by_internal};
use dioxus::prelude::*;
use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{SinkExt, StreamExt};
use gloo_net::websocket::{futures::WebSocket, Message};
use gloo_timers::future::TimeoutFuture;

/// 本地 mod 的 WebSocket 地址
const WS_URL: &str = "ws://127.0.0.1:32123/ws";
/// 重连退避：1s → 2s → … → 上限 30s
const RECONNECT_MIN_MS: u32 = 1_000;
const RECONNECT_MAX_MS: u32 = 30_000;
/// 应用层 JSON 心跳间隔
const PING_INTERVAL_MS: u32 = 5_000;
/// 连续多少次心跳周期未收到任何服务端消息判定为 stale
const STALE_MISSED_PINGS: u32 = 3;
/// 客户端版本（随 client.hello 上报）
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 连接 + 同步阶段合一的状态（对应 plan 7.1 的状态模型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    Disconnected,
    Connecting,
    ConnectedIdle,
    Queued,
    Requesting,
    Collecting,
    Synced,
    Error,
    Stale,
}

impl SyncStatus {
    fn label(self) -> &'static str {
        match self {
            SyncStatus::Disconnected => "未连接",
            SyncStatus::Connecting => "连接中",
            SyncStatus::ConnectedIdle => "已连接",
            SyncStatus::Queued => "已排队",
            SyncStatus::Requesting => "请求中",
            SyncStatus::Collecting => "采集中",
            SyncStatus::Synced => "已同步",
            SyncStatus::Error => "同步出错",
            SyncStatus::Stale => "连接超时",
        }
    }

    fn dot_class(self) -> &'static str {
        match self {
            SyncStatus::Disconnected => "off",
            SyncStatus::Stale | SyncStatus::Error => "err",
            SyncStatus::Synced => "synced",
            _ => "on",
        }
    }

    fn busy(self) -> bool {
        matches!(
            self,
            SyncStatus::Queued | SyncStatus::Requesting | SyncStatus::Collecting
        )
    }
}

#[derive(Debug, Clone)]
pub struct SyncResult {
    pub ok: bool,
    pub text: String,
    pub at: String,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: String,
    pub message: String,
}

/// 同步状态的全局 store。
#[derive(Debug, Clone, Copy)]
pub struct SyncStore {
    pub status: Signal<SyncStatus>,
    /// 同步阶段详情（如 "12/32 页"）
    pub phase_detail: Signal<Option<String>>,
    /// 同步进度 0.0..=1.0；None 表示没有进行中的同步
    pub progress: Signal<Option<f32>>,
    /// 最近一次同步结果（成功或失败）
    pub last_result: Signal<Option<SyncResult>>,
    /// 最近的事件日志（info/warn/error）
    pub logs: Signal<Vec<LogEntry>>,
    /// 服务端 hello 声明的能力
    pub capabilities: Signal<Vec<String>>,
    pub clients: Signal<usize>,
    /// 心跳未命中的周期数（收到任意消息即清零）
    pub missed: Signal<u32>,
    /// 出站命令通道（连接期间有效）
    pub send: Signal<Option<UnboundedSender<String>>>,
    /// 已收到的最大快照 seq（旧 seq 忽略）
    pub last_snapshot_seq: Signal<u64>,
    /// 刷新请求 ID 递增器
    pub request_seq: Signal<u64>,
    /// 同步详情面板开关
    pub show_detail: Signal<bool>,
}

fn console_warn(msg: &str) {
    web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(msg));
}

fn now_local_time() -> String {
    js_sys::Date::new_0().to_locale_time_string("zh-CN").into()
}

fn push_log(logs: &mut Vec<LogEntry>, level: &str, message: &str) {
    logs.push(LogEntry {
        level: level.to_string(),
        message: message.to_string(),
    });
    if logs.len() > 100 {
        logs.drain(0..logs.len() - 100);
    }
}

/// 把一批同步帕鲁全量替换进持有列表（保留手工条目）。返回汇总文本。
fn apply_snapshot(pals: &mut Vec<OwnedPal>, synced: Vec<SyncedPal>) -> String {
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
                let base = sync::strip_boss_prefix(&pal.species);
                let canonical = if db().pal(base).is_some() {
                    Some(base.to_string())
                } else {
                    db().canonical_name_ci(base).map(|s| s.to_string())
                };
                match canonical {
                    Some(c) => pal.species = c,
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
                    &format!(
                        "[sync] {}：剔除 {} 个未知被动 {:?}",
                        pal.species,
                        before - pal.passives.len(),
                        unknown
                    )
                    .into(),
                );
            }
            pal.passives.truncate(4);
            Some(pal)
        })
        .collect();
    let unrecognized = received - incoming.len();
    let added = sync::replace_synced(pals, incoming);

    let mut msg = format!("已同步替换 {added} 只");
    let mut skipped = Vec::new();
    if unrecognized > 0 {
        skipped.push(format!("{unrecognized} 只无法识别"));
    }
    if !skipped.is_empty() {
        msg.push_str(&format!("（{}已跳过）", skipped.join("、")));
    }
    msg
}

fn handle_server_text(text: &str, mut sync: SyncStore, mut store: OwnedStore) {
    match sync::parse_server_message(text) {
        Ok(ev) => match ev {
            ServerEvent::Hello {
                capabilities,
                clients,
                ..
            } => {
                sync.capabilities.set(capabilities);
                sync.clients.set(clients);
                if *sync.status.peek() == SyncStatus::Connecting {
                    sync.status.set(SyncStatus::ConnectedIdle);
                }
            }
            ServerEvent::SyncStatus {
                phase,
                requested_pages,
                total_pages,
                ..
            } => {
                let status = match phase.as_str() {
                    "queued" => SyncStatus::Queued,
                    "requesting" | "settling" => SyncStatus::Requesting,
                    "collecting" | "broadcasting" => SyncStatus::Collecting,
                    "complete" => SyncStatus::Synced,
                    "failed" => SyncStatus::Error,
                    _ => return,
                };
                sync.status.set(status);
                sync.phase_detail
                    .set(Some(format!("{requested_pages}/{total_pages} 页")));
                // 进度条：queued=0，requesting/collecting 按页推进，settling/broadcasting=满
                let p = match phase.as_str() {
                    "queued" => 0.0,
                    "requesting" if total_pages > 0 => {
                        (requested_pages as f32 / total_pages as f32).clamp(0.0, 1.0)
                    }
                    "requesting" => 0.0,
                    "settling" | "collecting" | "broadcasting" => 1.0,
                    "complete" | "failed" => {
                        sync.progress.set(None);
                        return;
                    }
                    _ => return,
                };
                sync.progress.set(Some(p));
            }
            ServerEvent::Snapshot {
                seq, pals, stats, ..
            } => {
                // 忽略旧序号（重连补发可能早于新快照）
                if seq < *sync.last_snapshot_seq.peek() {
                    return;
                }
                sync.last_snapshot_seq.set(seq);
                let summary = apply_snapshot(&mut store.pals.write(), pals);
                sync.last_result.set(Some(SyncResult {
                    ok: true,
                    text: summary,
                    at: now_local_time(),
                }));
                sync.phase_detail.set(Some(format!("{} 只 / {} 个容器", stats.total, stats.containers)));
                sync.progress.set(None);
                sync.status.set(SyncStatus::Synced);
            }
            ServerEvent::Log {
                level, message, ..
            } => {
                push_log(&mut sync.logs.write(), &level, &message);
            }
            ServerEvent::Error {
                code, message, ..
            } => {
                sync.last_result.set(Some(SyncResult {
                    ok: false,
                    text: message.clone(),
                    at: now_local_time(),
                }));
                push_log(&mut sync.logs.write(), "error", &format!("[{code}] {message}"));
                sync.progress.set(None);
                sync.status.set(SyncStatus::Error);
            }
            ServerEvent::Pong { .. } => {}
            ServerEvent::Unknown { .. } => {}
        },
        Err(e) => console_warn(&e.to_string()),
    }
}

/// 显式刷新：向服务端发送 `snapshot.request`（走命令队列，与 F7 同一同步接口）。
pub fn request_refresh(mut sync: SyncStore) {
    let allowed = sync
        .capabilities
        .read()
        .iter()
        .any(|c| c == "snapshot.request");
    if !allowed {
        return;
    }
    if let Some(tx) = sync.send.read().as_ref() {
        let mut n = sync.request_seq.write();
        *n += 1;
        let id = format!("req-{}", *n);
        drop(n);
        let _ = tx.unbounded_send(sync::snapshot_request_json(&id));
        sync.progress.set(Some(0.0));
        sync.status.set(SyncStatus::Queued);
    }
}

/// 启动 WebSocket 客户端（App 挂载时调用一次）。
pub fn use_ws_sync() {
    let mut sync = use_context::<SyncStore>();
    let store = use_context::<OwnedStore>();
    use_hook(move || {
        // 心跳任务：周期发 ping + 判定 stale（与连接生命周期无关）
        spawn(async move {
            let mut ping_id = 0u32;
            loop {
                TimeoutFuture::new(PING_INTERVAL_MS).await;
                let mut missed = sync.missed.write();
                *missed += 1;
                let stale = *missed >= STALE_MISSED_PINGS;
                drop(missed);
                if stale {
                    let st = *sync.status.peek();
                    if !matches!(st, SyncStatus::Disconnected | SyncStatus::Stale) {
                        sync.status.set(SyncStatus::Stale);
                    }
                }
                if let Some(tx) = sync.send.read().as_ref() {
                    ping_id += 1;
                    let _ = tx.unbounded_send(sync::ping_json(&format!("ping-{ping_id}")));
                }
            }
        });

        // 连接任务：指数退避重连；连接期间维护出站命令通道
        spawn(async move {
            let mut delay_ms = RECONNECT_MIN_MS;
            loop {
                let (tx, rx) = unbounded::<String>();
                sync.send.set(Some(tx));
                match WebSocket::open(WS_URL) {
                    Ok(ws) => {
                        sync.status.set(SyncStatus::Connecting);
                        sync.missed.set(0);
                        let (mut sink, stream) = ws.split();
                        let mut stream = stream.fuse();
                        if sink
                            .send(Message::Text(sync::client_hello_json(CLIENT_VERSION)))
                            .await
                            .is_ok()
                        {
                            sync.status.set(SyncStatus::ConnectedIdle);
                            let mut cmd_rx = rx.fuse();
                            loop {
                                futures_util::select! {
                                    msg = stream.next() => {
                                        match msg {
                                            Some(Ok(Message::Text(text))) => {
                                                sync.missed.set(0);
                                                handle_server_text(&text, sync, store);
                                            }
                                            Some(Ok(_)) => {}
                                            Some(Err(_)) | None => break,
                                        }
                                    }
                                    cmd = cmd_rx.next() => {
                                        match cmd {
                                            Some(c) => {
                                                if sink.send(Message::Text(c)).await.is_err() {
                                                    break;
                                                }
                                            }
                                            None => break,
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(_) => {}
                }
                sync.send.set(None);
                sync.capabilities.set(Vec::new());
                sync.clients.set(0);
                sync.status.set(SyncStatus::Disconnected);
                TimeoutFuture::new(delay_ms).await;
                delay_ms = (delay_ms * 2).min(RECONNECT_MAX_MS);
            }
        });
    });
}

/// 页脚状态栏：连接状态点 + 同步阶段 + 进度条 + 显式刷新。
#[component]
pub fn SyncStatusBar() -> Element {
    let sync = use_context::<SyncStore>();
    let status = *sync.status.read();
    let show_refresh = sync
        .capabilities
        .read()
        .iter()
        .any(|c| c == "snapshot.request");

    let detail = sync.phase_detail.read().clone();
    let progress = sync.progress.read().clone();
    let pct = progress.map(|p| (p.clamp(0.0, 1.0) * 100.0).round() as u32);
    let busy = status.busy();

    rsx! {
        div { class: "sync-status",
            span { class: "sync-status-dot sync-status-dot--{status.dot_class()}" }
            "游戏同步：{status.label()}"
            if let Some(d) = &detail {
                span { class: "sync-status-phase", "（{d}）" }
            }
            if let Some(pct) = pct {
                div { class: "sync-progress", title: "{pct}%",
                    div { class: "sync-progress-fill", style: "width: {pct}%" }
                }
            }
            if show_refresh {
                Button {
                    sm: true,
                    disabled: busy,
                    onclick: move |_| request_refresh(sync),
                    "刷新"
                }
            }
        }
    }
}

/// 右下角日志入口：一个悬浮按钮，点击展开/收起可滚动的日志面板。
#[component]
pub fn SyncConsole() -> Element {
    let sync = use_context::<SyncStore>();
    let show = *sync.show_detail.read();
    let mut show_detail = sync.show_detail;
    let logs = sync.logs.read();
    let result = sync.last_result.read().clone();
    let mut copied = use_signal(|| false);
    let count = logs.len();

    let diagnostic = {
        let mut s = String::from("pal-companion sync diagnostics\n");
        if let Some(r) = &result {
            s.push_str(&format!("last: {} {}\n", if r.ok { "ok" } else { "err" }, r.text));
        }
        for l in logs.iter().rev() {
            s.push_str(&format!("[{}] {}\n", l.level, l.message));
        }
        s
    };

    let copy = move |_| {
        if let Some(w) = web_sys::window() {
            let _ = w.navigator().clipboard().write_text(&diagnostic);
        }
        copied.set(true);
        let mut c = copied;
        spawn(async move {
            TimeoutFuture::new(1500).await;
            c.set(false);
        });
    };

    rsx! {
        if show {
            div { class: "sync-console",
                div { class: "sync-console-head",
                    span { class: "sync-console-title", "同步日志" }
                    Button {
                        variant: BtnVariant::Ghost,
                        sm: true,
                        onclick: copy,
                        if *copied.read() { "已复制" } else { "复制" }
                    }
                }
                if let Some(r) = &result {
                    div {
                        class: if r.ok { "sync-result sync-result--ok" } else { "sync-result sync-result--err" },
                        "{r.text}（{r.at}）"
                    }
                }
                div { class: "sync-console-log",
                    if logs.is_empty() {
                        div { class: "sync-console-empty", "暂无事件" }
                    }
                    for l in logs.iter().rev() {
                        div { class: "sync-console-entry sync-console-entry--{l.level}", "{l.message}" }
                    }
                }
            }
        }
        button {
            class: "sync-console-fab",
            title: "同步日志",
            onclick: move |_| {
                let next = !*show_detail.peek();
                show_detail.set(next);
            },
            svg {
                width: "16",
                height: "16",
                view_box: "0 0 24 24",
                fill: "none",
                stroke: "currentColor",
                stroke_width: "2",
                stroke_linecap: "round",
                stroke_linejoin: "round",
                polyline { points: "4 17 10 11 4 5" }
                line { x1: "12", y1: "19", x2: "20", y2: "19" }
            }
            if count > 0 {
                span { class: "sync-console-badge", "{count}" }
            }
        }
    }
}
