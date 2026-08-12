//! 我的帕鲁侧边栏：筛选 / 排序 / 添加 / 编辑 / 删除。

use crate::pages::calculator::pal_options;
use crate::planner::{Gender, OwnedPal, PalGroup, TargetGoal};
use crate::ui::{
    Badge, BadgeKind, BtnVariant, Button, ComboOption, Combobox, Dialog, Segment, Segmented,
};
use crate::{OwnedStore, SidebarState, TargetsStore, db, icon_url, passive_by_internal, passives};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

/// 导入/导出的数据封套（v2：含目标列表；兼容 v1 与裸数组）
#[derive(Debug, Serialize, Deserialize)]
struct ExportEnvelope {
    app: String,
    version: u32,
    pals: Vec<OwnedPal>,
    /// v1 无此字段 → None；None 时导入不动现有目标列表
    #[serde(default)]
    targets: Option<Vec<TargetGoal>>,
}

fn export_json(pals: &[OwnedPal], targets: &[TargetGoal]) -> String {
    serde_json::to_string_pretty(&ExportEnvelope {
        app: "pal-companion".to_string(),
        version: 2,
        pals: pals.to_vec(),
        targets: Some(targets.to_vec()),
    })
    .unwrap_or_default()
}

/// 解析导入文本：支持 v2/v1 封套或裸帕鲁数组。
/// 返回 (帕鲁, 目标)；目标为 None 表示数据中不含目标列表。
/// 物种必须在数据库中存在，未知被动静默剔除。
fn parse_import(text: &str) -> Result<(Vec<OwnedPal>, Option<Vec<TargetGoal>>), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("内容为空".to_string());
    }
    let (pals, targets) = match serde_json::from_str::<ExportEnvelope>(text) {
        Ok(e) => (e.pals, e.targets),
        Err(_) => (
            serde_json::from_str::<Vec<OwnedPal>>(text)
                .map_err(|e| format!("JSON 解析失败：{e}"))?,
            None,
        ),
    };
    let mut unknown: Vec<&str> = pals
        .iter()
        .map(|p| p.species.as_str())
        .filter(|s| db().pal(s).is_none())
        .collect();
    if let Some(ts) = &targets {
        unknown.extend(
            ts.iter()
                .map(|t| t.species.as_str())
                .filter(|s| db().pal(s).is_none()),
        );
    }
    if !unknown.is_empty() {
        return Err(format!(
            "包含无法识别的帕鲁：{}（可能来自不同数据版本）",
            unknown.join("、")
        ));
    }
    let pals = pals
        .into_iter()
        .map(|mut p| {
            p.passives.retain(|ps| passive_by_internal(ps).is_some());
            p
        })
        .collect();
    let targets = targets.map(|ts| {
        ts.into_iter()
            .map(|mut t| {
                t.desired_passives
                    .retain(|ps| passive_by_internal(ps).is_some());
                t
            })
            .collect()
    });
    Ok((pals, targets))
}

/// 复制到剪贴板。返回 false 表示环境不支持（非安全上下文等）。
fn copy_to_clipboard(text: &str) -> bool {
    let Some(w) = web_sys::window() else { return false };
    if !w.is_secure_context() {
        return false;
    }
    let _ = w.navigator().clipboard().write_text(text);
    true
}

/// 被动的稀有度展示（文字 + 配色）
pub fn passive_rarity(rank: i32) -> (String, BadgeKind) {
    match rank {
        r if r < 0 => ("负面".to_string(), BadgeKind::Negative),
        r if r >= 4 => ("传说".to_string(), BadgeKind::Gold),
        3 => ("稀有".to_string(), BadgeKind::Violet),
        _ => ("普通".to_string(), BadgeKind::Default),
    }
}

fn passive_options() -> Vec<ComboOption> {
    passives()
        .iter()
        .map(|p| ComboOption {
            value: p.internal_name.clone(),
            label: p.name_zh.clone(),
            sublabel: Some(p.name_en.clone()),
            icon: None,
            desc: Some(p.desc_zh.clone()),
            badge: Some(passive_rarity(p.rank)),
        })
        .collect()
}

pub fn passive_badge_kind(rank: i32) -> BadgeKind {
    match rank {
        r if r < 0 => BadgeKind::Negative,
        r if r >= 4 => BadgeKind::Gold,
        3 => BadgeKind::Violet,
        _ => BadgeKind::Default,
    }
}

fn matches_query(pal: &OwnedPal, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    let Some(sp) = db().pal(&pal.species) else {
        return false;
    };
    sp.name_zh.to_lowercase().contains(q)
        || sp.name_en.to_lowercase().contains(q)
        || sp.internal_name.to_lowercase().contains(q)
        || pal.passives.iter().any(|ps| {
            passive_by_internal(ps).is_some_and(|p| {
                p.name_zh.to_lowercase().contains(q) || p.name_en.to_lowercase().contains(q)
            })
        })
}

#[component]
pub fn OwnedSidebar() -> Element {
    let state = use_context::<SidebarState>();
    let store = use_context::<OwnedStore>();
    let targets = use_context::<TargetsStore>();
    let mut filter = use_signal(String::new);
    // 分组 tab：all / party / box / base（同步来的帕鲁带容器分组）
    let tab = use_signal(|| "all".to_string());
    // 据点子 tab："all" 或据点序号（base tab 下显示）
    let base_tab = use_signal(|| "all".to_string());
    let mut dialog_open = use_signal(|| false);
    let mut editing = use_signal(|| None::<OwnedPal>);
    let mut import_open = use_signal(|| false);
    // 剪贴板不可用时的兜底导出（弹窗手动复制）
    let mut fallback_open = use_signal(|| false);
    let mut fallback_json = use_signal(String::new);
    let mut copied = use_signal(|| false);

    let q = filter.read().to_lowercase();
    let group_tab = tab.read().clone();
    let base_filter = base_tab.read().clone();
    let all_pals = store.pals.read();
    // 各分组数量（未筛选统计，供 tab 徽标）
    let (mut n_party, mut n_box, mut n_base) = (0usize, 0usize, 0usize);
    let mut basecamps: Vec<u8> = Vec::new();
    for p in all_pals.iter() {
        match p.group {
            PalGroup::Party => n_party += 1,
            PalGroup::Box => n_box += 1,
            PalGroup::Base => {
                n_base += 1;
                if let Some(bc) = p.basecamp {
                    if !basecamps.contains(&bc) { basecamps.push(bc); }
                }
            }
        }
    }
    basecamps.sort_unstable();
    let total = all_pals.len();
    let mut list: Vec<OwnedPal> = all_pals
        .iter()
        .filter(|p| matches_query(p, &q))
        .filter(|p| {
            let ok_group = group_tab == "all"
                || (group_tab == "party" && p.group == PalGroup::Party)
                || (group_tab == "box" && p.group == PalGroup::Box)
                || (group_tab == "base" && p.group == PalGroup::Base);
            if !ok_group { return false; }
            // 据点子 tab 过滤（仅 base tab 且选了具体据点时生效）
            if group_tab == "base" && base_filter != "all" {
                return p.basecamp.map(|b| b.to_string()) == Some(base_filter.clone());
            }
            true
        })
        .cloned()
        .collect();
    // 统一按图鉴编号排序
    list.sort_by_key(|p| db().pal(&p.species).map(|s| s.paldex_no));

    // 据点子 tab 选项（rsx! 外构造，避免结构体字面量与 rsx! 语法冲突）
    let base_opts = if group_tab == "base" && !basecamps.is_empty() {
        let mut opts = vec![Segment { value: "all".to_string(), label: "全部据点".to_string() }];
        for bc in &basecamps {
            let n_bc = all_pals.iter().filter(|p| p.basecamp == Some(*bc)).count();
            opts.push(Segment {
                value: bc.to_string(),
                label: format!("据点 {bc} ({n_bc})"),
            });
        }
        Some(opts)
    } else {
        None
    };

    let open = *state.open.read();

    rsx! {
        aside { class: if open { "sidebar" } else { "sidebar sidebar--closed" },
            div { class: "sidebar-inner",
                div { class: "sidebar-head",
                    h2 { "我的帕鲁" }
                    span { class: "count", "{total}" }
                    Button {
                        variant: BtnVariant::Ghost,
                        sm: true,
                        icon: true,
                        class: "io-btn",
                        // 一键导出：直接复制 JSON（含目标列表）到剪贴板
                        onclick: move |_| {
                            let json = export_json(&store.pals.read(), &targets.goals.read());
                            if copy_to_clipboard(&json) {
                                copied.set(true);
                                let cb = wasm_bindgen::closure::Closure::once(move || {
                                    copied.set(false)
                                });
                                if let Some(w) = web_sys::window() {
                                    let _ = w
                                        .set_timeout_with_callback_and_timeout_and_arguments_0(
                                            cb.as_ref().unchecked_ref(),
                                            1500,
                                        );
                                }
                                cb.forget();
                            } else {
                                fallback_json.set(json);
                                fallback_open.set(true);
                            }
                        },
                        if *copied.read() {
                            "✓"
                        } else {
                            svg {
                                width: "14", height: "14", view_box: "0 0 24 24", fill: "none",
                                stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
                                path { d: "M12 15V3" }
                                path { d: "m7 8 5-5 5 5" }
                                path { d: "M5 15v4a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-4" }
                            }
                        }
                    }
                    Button {
                        variant: BtnVariant::Ghost,
                        sm: true,
                        icon: true,
                        class: "io-btn",
                        onclick: move |_| import_open.set(true),
                        svg {
                            width: "14", height: "14", view_box: "0 0 24 24", fill: "none",
                            stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
                            path { d: "M12 3v12" }
                            path { d: "m17 10-5 5-5-5" }
                            path { d: "M5 21h14" }
                        }
                    }
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
                input {
                    class: "input sidebar-filter",
                    placeholder: "筛选：帕鲁名 / 被动名…",
                    value: "{filter}",
                    oninput: move |e| filter.set(e.value()),
                }
                div { class: "sidebar-sort",
                    Segmented {
                        options: vec![
                            Segment { value: "all".to_string(), label: format!("全部 ({total})") },
                            Segment { value: "party".to_string(), label: format!("队伍 ({n_party})") },
                            Segment { value: "box".to_string(), label: format!("盒子 ({n_box})") },
                            Segment { value: "base".to_string(), label: format!("据点 ({n_base})") },
                        ],
                        value: tab,
                    }
                }
                // 据点子 tab：base 分组下按据点序号分组显示
                if let Some(opts) = &base_opts {
                    div { class: "sidebar-sort sidebar-sort--sub",
                        Segmented { options: opts.clone(), value: base_tab }
                    }
                }
                // 游戏同步提示条（有待确认列表时出现）
                crate::sync_client::SyncBanner {}
                div { class: "sidebar-list",
                    if list.is_empty() {
                        div { class: "sidebar-empty",
                            if total == 0 {
                                "还没有登记帕鲁，点击上方「+ 添加」开始。"
                            } else {
                                "没有符合筛选条件的帕鲁。"
                            }
                        }
                    }
                    for pal in list {
                        {
                            let Some(sp) = db().pal(&pal.species) else {
                                return rsx! {};
                            };
                            let gender_class = match pal.gender {
                                Gender::Male => "male",
                                Gender::Female => "female",
                            };
                            let p = pal.clone();
                            let fav_label = ["", "I", "II", "III"][pal.favorite.min(3) as usize].to_string();
                            rsx! {
                                button {
                                    key: "{pal.id}",
                                    class: "owned-item",
                                    onclick: move |_| {
                                        editing.set(Some(p.clone()));
                                        dialog_open.set(true);
                                    },
                                    div { class: "owned-avatar",
                                        img { src: icon_url(&pal.species), alt: "{sp.name_zh}" }
                                        // lucky 必然是头领形态，左上角只显示其一：闪光图标优先，否则头领
                                        if pal.is_lucky {
                                            img { class: "avatar-badge-img", src: "icons/rare.png", alt: "闪光" }
                                        } else if pal.is_boss {
                                            span { class: "avatar-badge avatar-badge--boss", "👑" }
                                        }
                                        if pal.favorite > 0 {
                                            img { class: "avatar-badge-img avatar-badge-img--fav", src: "icons/favomark.png", alt: "{fav_label}" }
                                        }
                                    }
                                    div { class: "owned-item-main",
                                        div { class: "owned-item-name",
                                            span { class: "dex-no", "#{sp.paldex_no}" }
                                            if let Some(nick) = &pal.nickname {
                                                span { class: "owned-item-nick", "{nick}" }
                                                span { class: "species-sub", "{sp.name_zh}" }
                                            } else {
                                                "{sp.name_zh}"
                                            }
                                            span { class: "gender-tag {gender_class}", "{pal.gender.symbol()}" }
                                        }
                                        if !pal.passives.is_empty() {
                                            div { class: "owned-item-passives",
                                                for ps in &pal.passives {
                                                    if let Some(pp) = passive_by_internal(ps) {
                                                        Badge { kind: passive_badge_kind(pp.rank), "{pp.name_zh}" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        PalFormDialog { open: dialog_open, editing: editing.read().clone() }
        ImportDialog { open: import_open }

        // 剪贴板不可用时的兜底导出弹窗（手动全选复制）
        Dialog {
            open: fallback_open,
            title: "导出".to_string(),
            description: "当前环境不支持自动复制，请全选以下内容手动复制".to_string(),
            textarea {
                class: "input textarea",
                readonly: true,
                value: "{fallback_json}",
                onclick: move |e| {
                    if let Some(el) = e.data().downcast::<web_sys::HtmlTextAreaElement>() {
                        el.select();
                    }
                },
            }
        }
    }
}

/// 导入对话框（同时处理帕鲁与目标列表）。
#[component]
fn ImportDialog(open: Signal<bool>) -> Element {
    let store = use_context::<OwnedStore>();
    let targets = use_context::<TargetsStore>();
    let mut pals = store.pals;
    let mut goals = targets.goals;
    let mut import_text = use_signal(String::new);
    let import_mode = use_signal(|| "append".to_string());
    let mut message = use_signal(|| None::<(bool, String)>);

    let do_import = move |_| {
        let text = import_text.read().clone();
        match parse_import(&text) {
            Ok((mut new_pals, new_targets)) => {
                let pal_count = new_pals.len();
                let replace = import_mode.read().as_str() == "replace";
                {
                    let mut list = pals.write();
                    if replace {
                        for (i, p) in new_pals.iter_mut().enumerate() {
                            p.id = (i + 1) as u64;
                        }
                        *list = new_pals;
                    } else {
                        let mut next = list.iter().map(|p| p.id).max().unwrap_or(0) + 1;
                        for mut p in new_pals {
                            p.id = next;
                            next += 1;
                            list.push(p);
                        }
                    }
                }
                // 数据里不含目标字段（v1 / 裸数组）时不动现有目标
                let target_count = new_targets.as_ref().map(|t| t.len());
                if let Some(mut new_targets) = new_targets {
                    let mut list = goals.write();
                    if replace {
                        for (i, t) in new_targets.iter_mut().enumerate() {
                            t.id = (i + 1) as u64;
                        }
                        *list = new_targets;
                    } else {
                        let mut next = list.iter().map(|t| t.id).max().unwrap_or(0) + 1;
                        for mut t in new_targets {
                            t.id = next;
                            next += 1;
                            list.push(t);
                        }
                    }
                }
                let mode_label = if replace { "覆盖" } else { "追加" };
                let detail = match target_count {
                    Some(tc) => format!("{pal_count} 只帕鲁、{tc} 个目标"),
                    None => format!("{pal_count} 只帕鲁"),
                };
                message.set(Some((true, format!("已{mode_label}导入 {detail}"))));
                import_text.set(String::new());
            }
            Err(e) => message.set(Some((false, e))),
        }
    };

    rsx! {
        Dialog {
            open,
            title: "导入".to_string(),
            description: "粘贴导出的 JSON（包含帕鲁与目标列表）".to_string(),
            div { class: "form-row",
                textarea {
                    class: "input textarea",
                    placeholder: "粘贴导出的 JSON…",
                    value: "{import_text}",
                    oninput: move |e| import_text.set(e.value()),
                }
                div { style: "margin-top: 8px; display: flex; gap: 8px; align-items: center; justify-content: flex-end;",
                    Segmented {
                        options: vec![
                            Segment { value: "append".to_string(), label: "追加".to_string() },
                            Segment { value: "replace".to_string(), label: "覆盖".to_string() },
                        ],
                        value: import_mode,
                    }
                    Button { sm: true, onclick: do_import, "导入" }
                }
            }
            if let Some((ok, msg)) = message.read().clone() {
                p { class: if ok { "io-message io-message--ok" } else { "io-message io-message--err" },
                    "{msg}"
                }
            }
        }
    }
}

/// 添加 / 编辑共用的表单对话框。
#[component]
fn PalFormDialog(open: Signal<bool>, editing: Option<OwnedPal>) -> Element {
    let store = use_context::<OwnedStore>();
    let mut pals = store.pals;

    let mut species = use_signal(|| None::<String>);
    let mut gender = use_signal(|| "male".to_string());
    let mut boss_lucky = use_signal(|| "none".to_string());
    let mut favorite = use_signal(|| "0".to_string());
    let mut nickname = use_signal(String::new);
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
                Some(p) => {
                    species.set(Some(p.species.clone()));
                    gender.set(
                        match p.gender {
                            Gender::Male => "male",
                            Gender::Female => "female",
                        }
                        .to_string(),
                    );
                    boss_lucky.set(if p.is_boss { "boss" } else if p.is_lucky { "lucky" } else { "none" }.to_string());
                    favorite.set(p.favorite.min(3).to_string());
                    nickname.set(p.nickname.clone().unwrap_or_default());
                    for (i, mut s) in slots.into_iter().enumerate() {
                        s.set(p.passives.get(i).cloned());
                    }
                }
                None => {
                    species.set(None);
                    gender.set("male".to_string());
                    boss_lucky.set("none".to_string());
                    favorite.set("0".to_string());
                    nickname.set(String::new());
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
        let passives: Vec<String> = slots.iter().filter_map(|s| s.read().clone()).collect();
        let g = if gender.read().as_str() == "male" {
            Gender::Male
        } else {
            Gender::Female
        };
        match &editing_for_save {
            Some(old) => {
                let mut list = pals.write();
                if let Some(entry) = list.iter_mut().find(|x| x.id == old.id) {
                    entry.species = sp;
                    entry.gender = g;
                    entry.passives = passives;
                    entry.is_boss = boss_lucky.read().as_str() == "boss";
                    entry.is_lucky = boss_lucky.read().as_str() == "lucky";
                    entry.favorite = favorite.read().parse().unwrap_or(0);
                    let n = nickname.read().trim().to_string();
                    entry.nickname = if n.is_empty() { None } else { Some(n) };
                }
            }
            None => {
                let mut list = pals.write();
                let id = list.iter().map(|p| p.id).max().unwrap_or(0) + 1;
                let n = nickname.read().trim().to_string();
                list.push(OwnedPal {
                    id,
                    species: sp,
                    gender: g,
                    passives,
                    group: PalGroup::Box,
                    is_boss: boss_lucky.read().as_str() == "boss",
                    is_lucky: boss_lucky.read().as_str() == "lucky",
                    favorite: favorite.read().parse().unwrap_or(0),
                    nickname: if n.is_empty() { None } else { Some(n) },
                    basecamp: None,
            synced: false,
                });
            }
        }
        open.set(false);
    };

    let editing_for_delete = editing.clone();
    let title = if is_edit { "编辑帕鲁" } else { "添加帕鲁" };

    rsx! {
        Dialog {
            open,
            title: title.to_string(),
            description: if is_edit { "修改种类、性别或被动技能".to_string() } else { "登记一只你拥有的帕鲁".to_string() },
            div { class: "form-row",
                label { class: "field-label", "种类" }
                Combobox { options: pal_options().to_vec(), value: species, placeholder: "搜索帕鲁…" }
            }
            div { class: "form-row",
                label { class: "field-label", "昵称（可留空）" }
                input {
                    class: "input",
                    value: "{nickname}",
                    placeholder: "游戏内昵称",
                    oninput: move |e| nickname.set(e.value()),
                }
            }
            div { class: "form-row form-inline",
                div { class: "form-inline-item",
                    label { class: "field-label", "性别" }
                    Segmented {
                        options: vec![
                            Segment { value: "male".to_string(), label: "♂ 雄性".to_string() },
                            Segment { value: "female".to_string(), label: "♀ 雌性".to_string() },
                        ],
                        value: gender,
                    }
                }
                div { class: "form-inline-item",
                    label { class: "field-label", "头领/幸运" }
                    Segmented {
                        options: vec![
                            Segment { value: "none".to_string(), label: "无".to_string() },
                            Segment { value: "boss".to_string(), label: "头领".to_string() },
                            Segment { value: "lucky".to_string(), label: "幸运".to_string() },
                        ],
                        value: boss_lucky,
                    }
                }
                div { class: "form-inline-item",
                    label { class: "field-label", "最爱" }
                    Segmented {
                        options: vec![
                            Segment { value: "0".to_string(), label: "无".to_string() },
                            Segment { value: "1".to_string(), label: "I".to_string() },
                            Segment { value: "2".to_string(), label: "II".to_string() },
                            Segment { value: "3".to_string(), label: "III".to_string() },
                        ],
                        value: favorite,
                    }
                }
            }
            div { class: "form-row",
                label { class: "field-label", "被动技能（最多 4 个，可留空）" }
                for (i, slot) in slots.into_iter().enumerate() {
                    div { key: "slot{i}", style: "margin-bottom: 8px;",
                        Combobox {
                            options: passive_options(),
                            value: slot,
                            placeholder: "被动技能 {i + 1}",
                        }
                        // 选中后显示被动的具体效果
                        if let Some(ps) = slot.read().as_ref() {
                            if let Some(pp) = passive_by_internal(ps) {
                                div { class: "passive-desc", "{pp.desc_zh}" }
                            }
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
                                pals.write().retain(|x| x.id != old.id);
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
