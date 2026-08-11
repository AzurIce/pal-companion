//! 路径规划页：左侧已持有帕鲁（全局侧边栏）、中间全屏图、右侧目标列表。

use crate::graph::layout::{GraphData, layout_plan};
use crate::pages::calculator::pal_options;
use crate::planner::{BreedKind, Gender, Plan, PlanError, PlanNode, PlanSource, TargetGoal, plan};
use crate::sidebar::passive_badge_kind;
use crate::ui::{Badge, BtnVariant, Button, ComboOption, Combobox, Dialog};
use crate::{OwnedStore, PlannerSideState, TargetsStore, db, icon_url, passive_by_internal, passives};
use dioxus::prelude::*;
use dioxus_flow::{EdgeEmphasis, FlowCanvas, NodeId, Size, Viewport, fit_viewport};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use wasm_bindgen::JsCast;

#[component]
pub fn PlannerPage() -> Element {
    let store = use_context::<OwnedStore>();
    let targets = use_context::<TargetsStore>();
    let selected = use_signal(|| None::<u64>);
    let mut viewport = use_signal(Viewport::default);
    let animate = use_signal(|| false);
    let hovered = use_signal(|| None::<String>);
    let shell_el = use_hook(|| Rc::new(RefCell::new(None::<web_sys::Element>)));

    // 选中目标 → 自动计算路径（持有列表或目标变更都会触发重算）
    let result = use_memo(move || {
        let id = (*selected.read())?;
        let goal = targets.goals.read().iter().find(|g| g.id == id).cloned()?;
        Some(plan(db(), &store.pals.read(), &goal.species, &goal.desired_passives))
    });

    let graph = use_memo(move || {
        result
            .read()
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .map(|p| layout_plan(&p.root))
    });

    // 新结果出来后自动适应视图
    let shell_for_effect = shell_el.clone();
    use_effect(move || {
        let g_guard = graph.read();
        if let Some(g) = g_guard.as_ref() {
            if let Some(el) = shell_for_effect.borrow().as_ref() {
                let rect = el.get_bounding_client_rect();
                let size = Size::new(rect.width(), rect.height());
                viewport.set(fit_viewport(&g.nodes, size, 56.0, 0.35, 1.4));
                flash_animate(animate);
            }
        }
    });

    // hover 祖先链
    let chain = use_memo(move || {
        let Some(h) = hovered.read().clone() else {
            return HashSet::new();
        };
        let g_guard = graph.read();
        let Some(g) = g_guard.as_ref() else {
            return HashSet::new();
        };
        // target -> [sources]（product -> ingredients）
        let mut parents: HashMap<&str, Vec<&str>> = HashMap::new();
        for e in &g.edges {
            parents
                .entry(e.target.0.as_str())
                .or_default()
                .push(e.source.0.as_str());
        }
        let mut set = HashSet::new();
        let mut stack = vec![h.as_str()];
        while let Some(cur) = stack.pop() {
            if set.insert(cur.to_string()) {
                if let Some(ps) = parents.get(cur) {
                    stack.extend(ps.iter().copied());
                }
            }
        }
        set
    });

    // 图区空状态提示
    let has_pals = !store.pals.read().is_empty();
    let has_goals = !targets.goals.read().is_empty();
    let empty_hint = if !has_goals {
        rsx! {
            div { class: "graph-empty",
                div { class: "title", "在右侧添加一个目标帕鲁" }
                div { "目标是你想要通过配种得到的帕鲁，可附带期望继承的被动。" }
            }
        }
    } else if !has_pals {
        rsx! {
            div { class: "graph-empty",
                div { class: "title", "先在左侧边栏登记你的帕鲁" }
                div { "路径规划需要知道你已经拥有哪些帕鲁。" }
            }
        }
    } else {
        match &*result.read() {
            None => rsx! {
                div { class: "graph-empty",
                    div { "在右侧列表选择一个目标，路径图将显示在这里。" }
                }
            },
            Some(Err(_)) => rsx! {
                div { class: "graph-empty",
                    div { "当前持有的帕鲁无法配出该目标，详见右侧。" }
                }
            },
            Some(Ok(_)) => rsx! {},
        }
    };

    rsx! {
        div { class: "planner-layout",
            PlanGraph {
                graph: graph.read().clone(),
                viewport,
                animate,
                hovered,
                chain: chain.read().clone(),
                shell_el: shell_el.clone(),
                empty: empty_hint,
                desired: {
                    let id = *selected.read();
                    id.and_then(|id| {
                        targets
                            .goals
                            .read()
                            .iter()
                            .find(|g| g.id == id)
                            .map(|g| g.desired_passives.clone())
                    })
                    .unwrap_or_default()
                },
            }
            TargetSidebar { selected, result, graph }
        }
    }
}

/// 右侧目标列表侧边栏。
#[component]
fn TargetSidebar(
    selected: Signal<Option<u64>>,
    result: Memo<Option<Result<Plan, PlanError>>>,
    graph: Memo<Option<GraphData>>,
) -> Element {
    let targets = use_context::<TargetsStore>();
    let side = use_context::<PlannerSideState>();
    let store = use_context::<OwnedStore>();
    let goals = targets.goals;
    let mut dialog_open = use_signal(|| false);
    let mut editing = use_signal(|| None::<TargetGoal>);

    let side_open = *side.open.read();
    let total = goals.read().len();
    let current = *selected.read();

    rsx! {
        aside { class: if side_open { "planner-side" } else { "planner-side planner-side--closed" },
            div { class: "planner-side-inner",
                div { class: "sidebar-head",
                    h2 { "目标" }
                    span { class: "count", "{total}" }
                    Button {
                        variant: BtnVariant::Outline,
                        sm: true,
                        onclick: move |_| {
                            editing.set(None);
                            dialog_open.set(true);
                        },
                        "+ 添加"
                    }
                }
                div { class: "sidebar-list",
                    if total == 0 {
                        div { class: "sidebar-empty", "还没有目标，点击上方「+ 添加」。" }
                    }
                    for goal in goals.read().iter() {
                        {
                            let Some(sp) = db().pal(&goal.species) else {
                                return rsx! {};
                            };
                            let id = goal.id;
                            let g = goal.clone();
                            let active = current == Some(id);
                            rsx! {
                                div {
                                    key: "{id}",
                                    class: if active { "owned-item target-item active" } else { "owned-item target-item" },
                                    onclick: move |_| selected.set(Some(id)),
                                    img { src: icon_url(&goal.species), alt: "{sp.name_zh}" }
                                    div { class: "owned-item-main",
                                        div { class: "owned-item-name", "{sp.name_zh}" }
                                        if !goal.desired_passives.is_empty() {
                                            div { class: "owned-item-passives",
                                                for ps in &goal.desired_passives {
                                                    if let Some(pp) = passive_by_internal(ps) {
                                                        Badge { kind: passive_badge_kind(pp.rank), "{pp.name_zh}" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    button {
                                        class: "edit-btn",
                                        onclick: move |e| {
                                            e.stop_propagation();
                                            editing.set(Some(g.clone()));
                                            dialog_open.set(true);
                                        },
                                        "✎"
                                    }
                                }
                            }
                        }
                    }
                }

                match &*result.read() {
                    Some(Ok(p)) => {
                        let bred_count = graph
                            .read()
                            .as_ref()
                            .map(|g| g.bred_count)
                            .unwrap_or(p.total_breedings as usize);
                        let used = graph
                            .read()
                            .as_ref()
                            .map(|g| g.used_owned.len())
                            .unwrap_or(p.used_owned.len());
                        // 期望被动中未被路径血统覆盖的（只能赌随机继承）
                        let missing_names: String = {
                            let goal = current.and_then(|id| {
                                goals.read().iter().find(|g| g.id == id).cloned()
                            });
                            let used_passives: Vec<String> = store
                                .pals
                                .read()
                                .iter()
                                .filter(|pal| p.used_owned.contains(&pal.id))
                                .flat_map(|pal| pal.passives.clone())
                                .collect();
                            goal.map(|g| g.desired_passives)
                                .unwrap_or_default()
                                .into_iter()
                                .filter(|d| !used_passives.contains(d))
                                .filter_map(|ps| {
                                    passive_by_internal(&ps).map(|p| p.name_zh.clone())
                                })
                                .collect::<Vec<_>>()
                                .join("、")
                        };
                        rsx! {
                            div { class: "side-stats",
                                div { class: "stat",
                                    b { "{bred_count}" }
                                    span { "总配种次数" }
                                }
                                div { class: "stat",
                                    b { "{p.generations}" }
                                    span { "世代深度" }
                                }
                                div { class: "stat",
                                    b { "{used}" }
                                    span { "用到已持有帕鲁" }
                                }
                            }
                            if !missing_names.is_empty() {
                                p { class: "side-note side-note--warn",
                                    "注意：{missing_names} 不在路径中任何已持有帕鲁身上，只能靠随机继承碰运气。"
                                }
                            }
                            p { class: "side-note", "图中相同子树已折叠复用；配种产物的性别按均可获得处理；被动继承为概率机制，路径仅作亲本优选。" }
                        }
                    }
                    Some(Err(PlanError::Unreachable { unique_parents, .. })) => {
                        let pairs = unique_parents.clone();
                        rsx! {
                            div { class: "side-error",
                                h3 { "无法配出目标" }
                                if pairs.is_empty() {
                                    p { "该帕鲁无法通过配种获得，请尝试野外捕捉。" }
                                } else {
                                    p { "它只能通过以下唯一组合产出，先去获得其中一对亲本：" }
                                    ul { class: "side-pairs",
                                        for (a, b) in pairs {
                                            {
                                                let pa = db().pal(&a).unwrap();
                                                let pb = db().pal(&b).unwrap();
                                                rsx! {
                                                    li { key: "{a}+{b}",
                                                        img { src: icon_url(&a), alt: "" }
                                                        span { "{pa.name_zh}" }
                                                        span { class: "pair-plus", "+" }
                                                        img { src: icon_url(&b), alt: "" }
                                                        span { "{pb.name_zh}" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => rsx! {},
                }
            }
        }

        TargetFormDialog { open: dialog_open, editing: editing.read().clone(), selected }
    }
}

/// 目标添加 / 编辑对话框（物种 + 期望被动）。
#[component]
fn TargetFormDialog(
    open: Signal<bool>,
    editing: Option<TargetGoal>,
    mut selected: Signal<Option<u64>>,
) -> Element {
    let targets = use_context::<TargetsStore>();
    let mut goals = targets.goals;

    let mut species = use_signal(|| None::<String>);
    let slots: [Signal<Option<String>>; 4] = [
        use_signal(|| None),
        use_signal(|| None),
        use_signal(|| None),
        use_signal(|| None),
    ];

    // 打开时按编辑对象初始化表单
    let editing_for_effect = editing.clone();
    use_effect(move || {
        if *open.read() {
            match &editing_for_effect {
                Some(g) => {
                    species.set(Some(g.species.clone()));
                    for (i, mut s) in slots.into_iter().enumerate() {
                        s.set(g.desired_passives.get(i).cloned());
                    }
                }
                None => {
                    species.set(None);
                    for mut s in slots {
                        s.set(None);
                    }
                }
            }
        }
    });

    let is_edit = editing.is_some();
    let can_save = species.read().is_some();
    let editing_for_save = editing.clone();

    let save = move |_| {
        let Some(sp) = species.read().clone() else { return };
        let desired: Vec<String> = slots.iter().filter_map(|s| s.read().clone()).collect();
        match &editing_for_save {
            Some(old) => {
                let mut list = goals.write();
                if let Some(entry) = list.iter_mut().find(|x| x.id == old.id) {
                    *entry = TargetGoal {
                        id: old.id,
                        species: sp,
                        desired_passives: desired,
                    };
                }
            }
            None => {
                let mut list = goals.write();
                let id = list.iter().map(|g| g.id).max().unwrap_or(0) + 1;
                list.push(TargetGoal {
                    id,
                    species: sp,
                    desired_passives: desired,
                });
                drop(list);
                selected.set(Some(id)); // 新增后直接选中
            }
        }
        open.set(false);
    };

    let editing_for_delete = editing.clone();
    let title = if is_edit { "编辑目标" } else { "添加目标" };

    // 期望被动只列正面被动
    let passive_options: Vec<ComboOption> = passives()
        .iter()
        .filter(|p| p.rank > 0)
        .map(|p| ComboOption {
            value: p.internal_name.clone(),
            label: p.name_zh.clone(),
            sublabel: Some(p.name_en.clone()),
            icon: None,
            desc: Some(p.desc_zh.clone()),
            badge: Some(crate::sidebar::passive_rarity(p.rank)),
        })
        .collect();

    rsx! {
        Dialog {
            open,
            title: title.to_string(),
            description: "想要通过配种得到的帕鲁，可指定期望继承的被动".to_string(),
            div { class: "form-row",
                label { class: "field-label", "目标帕鲁" }
                Combobox { options: pal_options().to_vec(), value: species, placeholder: "搜索帕鲁…" }
            }
            div { class: "form-row",
                label { class: "field-label", "期望继承的被动（最多 4 个，可留空）" }
                for (i, slot) in slots.into_iter().enumerate() {
                    div { key: "slot{i}", style: "margin-bottom: 8px;",
                        Combobox {
                            options: passive_options.clone(),
                            value: slot,
                            placeholder: "被动技能 {i + 1}",
                        }
                    }
                }
            }
            div { class: "dialog-actions",
                if is_edit {
                    Button {
                        variant: BtnVariant::Destructive,
                        onclick: move |_| {
                            if let Some(old) = &editing_for_delete {
                                goals.write().retain(|x| x.id != old.id);
                                if *selected.read() == Some(old.id) {
                                    selected.set(None);
                                }
                            }
                            open.set(false);
                        },
                        "删除"
                    }
                }
                Button {
                    variant: BtnVariant::Outline,
                    onclick: move |_| open.set(false),
                    "取消"
                }
                Button {
                    disabled: !can_save,
                    onclick: save,
                    "保存"
                }
            }
        }
    }
}

/// 短暂开启视口过渡动画（约 400ms 后关闭，避免影响拖拽手感）。
fn flash_animate(mut animate: Signal<bool>) {
    animate.set(true);
    let cb = wasm_bindgen::closure::Closure::once(move || animate.set(false));
    if let Some(w) = web_sys::window() {
        let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            400,
        );
    }
    cb.forget();
}

#[component]
fn PlanGraph(
    graph: Option<GraphData>,
    viewport: Signal<Viewport>,
    animate: Signal<bool>,
    mut hovered: Signal<Option<String>>,
    chain: HashSet<String>,
    shell_el: Rc<RefCell<Option<web_sys::Element>>>,
    empty: Element,
    /// 当前目标的期望被动（用于目标节点约束展示与可继承排序）
    desired: Vec<String>,
) -> Element {
    let side = use_context::<PlannerSideState>();
    let store = use_context::<OwnedStore>();
    let mut side_open = side.open;
    // 平移拖拽期间抑制 hover 更新（无关渲染会用旧 signal 值重写 style 造成瞬跳）
    let mut panning = use_signal(|| false);
    let (nodes, base_edges, info, root_id) = match graph {
        Some(g) => (g.nodes, g.edges, g.info, g.root_id),
        None => (Vec::new(), Vec::new(), HashMap::new(), String::new()),
    };
    let info = Rc::new(info);
    let highlight_active = !chain.is_empty();

    // 每个节点的可继承被动 = 其已持有祖先节点的被动并集
    let lineage: Rc<HashMap<String, Vec<String>>> = Rc::new({
        let mut parents: HashMap<String, Vec<String>> = HashMap::new();
        for e in &base_edges {
            parents
                .entry(e.target.0.clone())
                .or_default()
                .push(e.source.0.clone());
        }
        fn walk(
            id: &str,
            info: &HashMap<String, PlanNode>,
            parents: &HashMap<String, Vec<String>>,
            store: &OwnedStore,
            cache: &mut HashMap<String, Vec<String>>,
        ) -> Vec<String> {
            if let Some(v) = cache.get(id) {
                return v.clone();
            }
            let v = match info.get(id).map(|n| &n.source) {
                Some(PlanSource::Owned { pal_id }) => store
                    .pals
                    .read()
                    .iter()
                    .find(|p| p.id == *pal_id)
                    .map(|p| p.passives.clone())
                    .unwrap_or_default(),
                _ => {
                    let mut acc: Vec<String> = Vec::new();
                    if let Some(ps) = parents.get(id) {
                        for pid in ps {
                            for x in walk(pid, info, parents, store, cache) {
                                if !acc.contains(&x) {
                                    acc.push(x);
                                }
                            }
                        }
                    }
                    acc
                }
            };
            cache.insert(id.to_string(), v.clone());
            v
        }
        let mut cache = HashMap::new();
        info.keys()
            .map(|id| (id.clone(), walk(id, &info, &parents, &store, &mut cache)))
            .collect()
    });
    let desired = Rc::new(desired);

    let edges = base_edges
        .iter()
        .map(|e| {
            let mut e = e.clone();
            if highlight_active {
                e.emphasis = if chain.contains(&e.target.0) && chain.contains(&e.source.0) {
                    EdgeEmphasis::Highlight
                } else {
                    EdgeEmphasis::Dim
                };
            }
            e
        })
        .collect::<Vec<_>>();

    let info_for_render = info.clone();
    let chain_for_render = chain.clone();
    let lineage_for_render = lineage.clone();
    let desired_for_render = desired.clone();
    let render_node = move |id: NodeId| {
        let Some(node) = info_for_render.get(&id.0) else {
            return rsx! {};
        };
        let Some(p) = db().pal(&node.species) else {
            return rsx! {};
        };
        let is_owned = matches!(node.source, PlanSource::Owned { .. });
        let is_target = id.0 == root_id;
        let mut class = String::from("flow-node-card");
        if is_owned {
            class.push_str(" owned");
        }
        if is_target {
            class.push_str(" target");
        }
        if highlight_active {
            if chain_for_render.contains(&id.0) {
                class.push_str(" highlight");
            } else {
                class.push_str(" dim");
            }
        }
        let gender = node.need_gender;
        let gender_class = match gender {
            Some(Gender::Male) => "male",
            Some(Gender::Female) => "female",
            None => "",
        };
        let kind_label = match &node.source {
            PlanSource::Owned { .. } => "已持有",
            PlanSource::Bred { kind, .. } => match kind {
                BreedKind::Formula => "配种",
                BreedKind::Unique => "唯一组合",
                BreedKind::GenderUnique => "性别组合",
            },
        };

        // 被动行：已持有=实际被动；目标=约束被动（标覆盖情况）；配种节点=可继承被动
        const MAX_BADGES: usize = 4;
        let covered_set = lineage_for_render.get(&id.0).cloned().unwrap_or_default();
        let badges: Vec<(String, String)> = if is_target {
            // (internal_name, css class)：未被路径覆盖的用 missing 样式
            desired_for_render
                .iter()
                .map(|ps| {
                    let cls = if covered_set.contains(ps) {
                        ""
                    } else {
                        " badge--missing"
                    };
                    (ps.clone(), cls.to_string())
                })
                .collect()
        } else if let PlanSource::Owned { .. } = &node.source {
            covered_set.iter().map(|ps| (ps.clone(), String::new())).collect()
        } else {
            // 可继承：期望被动排前
            let mut v = covered_set.clone();
            v.sort_by_key(|ps| {
                desired_for_render
                    .iter()
                    .position(|d| d == ps)
                    .unwrap_or(usize::MAX)
            });
            v.into_iter().map(|ps| (ps, String::new())).collect()
        };
        let overflow = badges.len().saturating_sub(MAX_BADGES);
        // 悬停显示全部被动（含被行数限制隐藏的）
        let all_names = badges
            .iter()
            .filter_map(|(ps, _)| passive_by_internal(ps).map(|p| p.name_zh.clone()))
            .collect::<Vec<_>>()
            .join("、");

        let id_enter = id.0.clone();
        rsx! {
            div {
                class: "{class}",
                onmouseenter: move |_| {
                    if !*panning.read() {
                        hovered.set(Some(id_enter.clone()));
                    }
                },
                onmouseleave: move |_| {
                    if !*panning.read() {
                        hovered.set(None);
                    }
                },
                img { src: icon_url(&node.species), alt: "{p.name_zh}" }
                div { class: "node-main",
                    div { class: "name", "{p.name_zh}" }
                    div { class: "sub",
                        if let Some(g) = gender {
                            span { class: "gender-tag {gender_class}", "{g.symbol()}" }
                        }
                        span { "{kind_label}" }
                    }
                    if !badges.is_empty() {
                        div { class: "node-passives", title: "{all_names}",
                            for (ps, extra_cls) in badges.iter().take(MAX_BADGES) {
                                if let Some(pp) = passive_by_internal(ps) {
                                    if extra_cls.is_empty() {
                                        Badge { kind: passive_badge_kind(pp.rank), "{pp.name_zh}" }
                                    } else {
                                        span { class: "badge {extra_cls}", title: "{pp.desc_zh}", "{pp.name_zh}" }
                                    }
                                }
                            }
                            if overflow > 0 {
                                span { class: "badge", "+{overflow}" }
                            }
                        }
                    }
                }
            }
        }
    };

    let nodes_for_fit = nodes.clone();
    let shell_for_mount = shell_el.clone();
    let shell_for_fit = shell_el.clone();
    rsx! {
        div {
            class: "planner-graph",
            onmounted: move |event| {
                *shell_for_mount.borrow_mut() =
                    event.data().downcast::<web_sys::Element>().cloned();
            },
            div { class: "graph-toolbar",
                Button {
                    variant: BtnVariant::Outline,
                    sm: true,
                    onclick: move |_| {
                        if let Some(el) = shell_for_fit.borrow().as_ref() {
                            let rect = el.get_bounding_client_rect();
                            let size = Size::new(rect.width(), rect.height());
                            viewport.set(fit_viewport(&nodes_for_fit, size, 56.0, 0.35, 1.4));
                            flash_animate(animate);
                        }
                    },
                    "适应视图"
                }
                Button {
                    variant: BtnVariant::Outline,
                    sm: true,
                    icon: true,
                    onclick: move |_| {
                        let next = !*side_open.read();
                        side_open.set(next);
                    },
                    svg {
                        width: "14", height: "14", view_box: "0 0 24 24", fill: "none",
                        stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
                        rect { x: "3", y: "3", width: "18", height: "18", rx: "2" }
                        line { x1: "15", y1: "3", x2: "15", y2: "21" }
                    }
                }
            }
            FlowCanvas {
                nodes,
                edges,
                viewport,
                render_node,
                on_node_move: |_| {},
                on_node_click: |_| {},
                animate: *animate.read(),
                edge_color: "var(--edge)",
                on_pan_start: move |_| panning.set(true),
                on_pan_end: move |_| panning.set(false),
                empty,
            }
        }
    }
}
