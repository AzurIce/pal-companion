mod breeding;
mod graph;
mod pages;
mod planner;
mod sidebar;
mod storage;
mod sync;
mod sync_client;
mod theme;
mod ui;

use breeding::{BreedingDB, Passive};
use dioxus::prelude::*;
use pages::{Calculator, PlannerPage};
use planner::OwnedPal;
use sidebar::OwnedSidebar;
use std::sync::OnceLock;
use ui::ThemeToggle;

fn main() {
    dioxus::launch(App);
}

/// 打包进 WASM 的配种数据库（懒解析一次）。
pub fn db() -> &'static BreedingDB {
    static DB: OnceLock<BreedingDB> = OnceLock::new();
    DB.get_or_init(|| {
        breeding::from_json(
            include_str!("../assets/data/pals.json"),
            include_str!("../assets/data/unique_combos.json"),
        )
        .expect("内置配种数据解析失败")
    })
}

/// 被动技能列表（按 rank 降序）。
pub fn passives() -> &'static [Passive] {
    static P: OnceLock<Vec<Passive>> = OnceLock::new();
    P.get_or_init(|| {
        serde_json::from_str(include_str!("../assets/data/passives.json")).expect("被动数据解析失败")
    })
}

pub fn passive_by_internal(internal: &str) -> Option<&'static Passive> {
    passives().iter().find(|p| p.internal_name == internal)
}

/// 帕鲁图标 URL（public/ 目录由 dx 原样拷贝到输出根目录）。
pub fn icon_url(internal_name: &str) -> String {
    format!("icons/{internal_name}.png")
}

/// 已持有帕鲁的全局 store（localStorage 持久化）。
#[derive(Debug, Clone, Copy)]
pub struct OwnedStore {
    pub pals: Signal<Vec<OwnedPal>>,
}

/// 规划目标列表的全局 store（localStorage 持久化）。
#[derive(Debug, Clone, Copy)]
pub struct TargetsStore {
    pub goals: Signal<Vec<planner::TargetGoal>>,
}

/// 侧边栏开合状态（持久化）。
#[derive(Debug, Clone, Copy)]
pub struct SidebarState {
    pub open: Signal<bool>,
}

/// 规划页右侧目标栏开合状态（持久化）。
#[derive(Debug, Clone, Copy)]
pub struct PlannerSideState {
    pub open: Signal<bool>,
}

#[derive(Routable, Clone, PartialEq)]
enum Route {
    #[layout(Navbar)]
    #[route("/")]
    Calculator {},
    #[route("/planner")]
    PlannerPage {},
}

#[component]
fn App() -> Element {
    let theme = theme::use_theme();
    use_context_provider(|| theme);
    let owned = storage::use_persistent("pal-companion:owned", Vec::<OwnedPal>::new);
    use_context_provider(|| OwnedStore { pals: owned });
    let targets =
        storage::use_persistent("pal-companion:targets", Vec::<planner::TargetGoal>::new);
    use_context_provider(|| TargetsStore { goals: targets });
    let sidebar_open = storage::use_persistent("pal-companion:sidebar", || true);
    use_context_provider(|| SidebarState { open: sidebar_open });
    let planner_side_open = storage::use_persistent("pal-companion:planner-side", || true);
    use_context_provider(|| PlannerSideState {
        open: planner_side_open,
    });
    // 游戏同步：状态 store + WebSocket 客户端（本地 mod 未启动时静默重试）
    // 自动同步开关（key 沿用旧名 palcompanion_auto_merge 以保留用户已有状态）
    let auto_sync = storage::use_persistent("palcompanion_auto_merge", || false);
    use_context_provider(|| sync_client::SyncStore {
        status: Signal::new(sync_client::SyncStatus::Disconnected),
        pending: Signal::new(Vec::new()),
        auto_sync,
        toast: Signal::new(None),
    });
    sync_client::use_ws_sync();
    rsx! {
        style { {include_str!("../assets/main.css")} }
        Router::<Route> {}
    }
}

#[component]
fn Navbar() -> Element {
    let sidebar = use_context::<SidebarState>();
    let mut open = sidebar.open;
    rsx! {
        div { class: "app-shell",
            header { class: "navbar",
                div { class: "brand",
                    span { "帕鲁助手" }
                }
                nav {
                    Link { to: Route::Calculator {}, class: "nav-link", active_class: "active", "配种查询" }
                    Link { to: Route::PlannerPage {}, class: "nav-link", active_class: "active", "路径规划" }
                }
                a {
                    class: "gh-link",
                    href: "https://github.com/AzurIce/pal-companion",
                    target: "_blank",
                    title: "GitHub 仓库",
                    svg {
                        width: "18", height: "18", view_box: "0 0 24 24", fill: "currentColor",
                        path { d: "M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12" }
                    }
                }
                ThemeToggle {}
            }
            div { class: "layout-row",
                OwnedSidebar {}
                main { class: "content",
                    Outlet::<Route> {}
                }
                button {
                    class: if *open.read() { "sidebar-float-toggle sidebar-float-toggle--open" } else { "sidebar-float-toggle" },
                    title: "展开 / 收起侧边栏",
                    onclick: move |_| {
                        let next = !*open.read();
                        open.set(next);
                    },
                    svg {
                        width: "14", height: "14", view_box: "0 0 24 24", fill: "none",
                        stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
                        rect { x: "3", y: "3", width: "18", height: "18", rx: "2" }
                        line { x1: "9", y1: "3", x2: "9", y2: "21" }
                    }
                }
            }
            footer { class: "attribution statusbar",
                // 页脚改为状态栏：左侧同步状态 + 自动同步开关，右侧素材说明
                sync_client::SyncStatusBar {}
                span { class: "attribution-text",
                    "数据来自 "
                    a { href: "https://github.com/tylercamp/palcalc", "palcalc" }
                    " (MIT)，配种规则经游戏数据穷举表 100% 回归校验。帕鲁名称与图像素材 © Pocketpair。"
                }
            }
            sync_client::SyncToast {}
        }
    }
}
