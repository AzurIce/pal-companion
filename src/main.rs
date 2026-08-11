mod breeding;
mod graph;
mod pages;
mod planner;
mod sidebar;
mod storage;
mod theme;
mod ui;

use breeding::{BreedingDB, Passive};
use dioxus::prelude::*;
use pages::{Calculator, PlannerPage};
use planner::OwnedPal;
use sidebar::OwnedSidebar;
use std::sync::OnceLock;
use ui::{BtnVariant, Button, ThemeToggle};

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
        header { class: "navbar",
            Button {
                variant: BtnVariant::Ghost,
                icon: true,
                class: "sidebar-toggle",
                onclick: move |_| {
                    let next = !*open.read();
                    open.set(next);
                },
                svg {
                    width: "16", height: "16", view_box: "0 0 24 24", fill: "none",
                    stroke: "currentColor", stroke_width: "2", stroke_linecap: "round",
                    line { x1: "3", y1: "6", x2: "21", y2: "6" }
                    line { x1: "3", y1: "12", x2: "21", y2: "12" }
                    line { x1: "3", y1: "18", x2: "21", y2: "18" }
                }
            }
            div { class: "brand",
                span { "帕鲁助手" }
            }
            nav {
                Link { to: Route::Calculator {}, class: "nav-link", active_class: "active", "配种查询" }
                Link { to: Route::PlannerPage {}, class: "nav-link", active_class: "active", "路径规划" }
            }
            ThemeToggle {}
        }
        div { class: "layout-row",
            OwnedSidebar {}
            main { class: "content",
                Outlet::<Route> {}
            }
        }
        footer { class: "attribution",
            "数据来自 "
            a { href: "https://github.com/tylercamp/palcalc", "palcalc" }
            " (MIT)，配种规则经游戏数据穷举表 100% 回归校验。帕鲁名称与图像素材 © Pocketpair。"
        }
    }
}
