//! 我的帕鲁侧边栏：筛选 / 排序 / 添加 / 编辑 / 删除。

use crate::pages::calculator::pal_options;
use crate::planner::{Gender, OwnedPal};
use crate::ui::{
    Badge, BadgeKind, BtnVariant, Button, ComboOption, Combobox, Dialog, Segment, Segmented,
};
use crate::{OwnedStore, SidebarState, db, icon_url, passive_by_internal, passives};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

/// 导入/导出的数据封套
#[derive(Debug, Serialize, Deserialize)]
struct ExportEnvelope {
    app: String,
    version: u32,
    pals: Vec<OwnedPal>,
}

fn export_json(pals: &[OwnedPal]) -> String {
    serde_json::to_string_pretty(&ExportEnvelope {
        app: "pal-companion".to_string(),
        version: 1,
        pals: pals.to_vec(),
    })
    .unwrap_or_default()
}

/// 解析导入文本：支持封套或裸数组；物种必须在数据库中存在，未知被动静默剔除。
fn parse_import(text: &str) -> Result<Vec<OwnedPal>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("内容为空".to_string());
    }
    let pals: Vec<OwnedPal> = serde_json::from_str::<ExportEnvelope>(text)
        .map(|e| e.pals)
        .or_else(|_| serde_json::from_str::<Vec<OwnedPal>>(text))
        .map_err(|e| format!("JSON 解析失败：{e}"))?;
    let unknown: Vec<&str> = pals
        .iter()
        .map(|p| p.species.as_str())
        .filter(|s| db().pal(s).is_none())
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "包含无法识别的帕鲁：{}（可能来自不同数据版本）",
            unknown.join("、")
        ));
    }
    Ok(pals
        .into_iter()
        .map(|mut p| {
            p.passives
                .retain(|ps| passive_by_internal(ps).is_some());
            p
        })
        .collect())
}

fn copy_to_clipboard(text: &str) -> bool {
    let Some(w) = web_sys::window() else { return false };
    if !w.is_secure_context() {
        return false;
    }
    let _ = w.navigator().clipboard().write_text(text);
    true
}

fn passive_options() -> Vec<ComboOption> {
    passives()
        .iter()
        .map(|p| ComboOption {
            value: p.internal_name.clone(),
            label: p.name_zh.clone(),
            sublabel: Some(p.name_en.clone()),
            icon: None,
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
    let mut filter = use_signal(String::new);
    let sort = use_signal(|| "dex".to_string());
    let mut dialog_open = use_signal(|| false);
    let mut editing = use_signal(|| None::<OwnedPal>);
    let mut io_open = use_signal(|| false);

    let q = filter.read().to_lowercase();
    let mut list: Vec<OwnedPal> = store
        .pals
        .read()
        .iter()
        .filter(|p| matches_query(p, &q))
        .cloned()
        .collect();
    match sort.read().as_str() {
        "name" => list.sort_by(|a, b| {
            let da = db().pal(&a.species);
            let db_ = db().pal(&b.species);
            da.map(|p| &p.name_zh).cmp(&db_.map(|p| &p.name_zh))
        }),
        "power" => list.sort_by_key(|p| db().pal(&p.species).map(|s| s.breeding_power)),
        "added" => list.sort_by_key(|p| std::cmp::Reverse(p.id)),
        _ => list.sort_by_key(|p| db().pal(&p.species).map(|s| s.paldex_no)),
    }

    let open = *state.open.read();
    let total = store.pals.read().len();

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
                        onclick: move |_| io_open.set(true),
                        svg {
                            width: "14", height: "14", view_box: "0 0 24 24", fill: "none",
                            stroke: "currentColor", stroke_width: "2", stroke_linecap: "round", stroke_linejoin: "round",
                            path { d: "M12 3v12" }
                            path { d: "m7 8 5-5 5 5" }
                            path { d: "M12 21V9" }
                            path { d: "m17 16-5 5-5-5" }
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
                            Segment { value: "dex".to_string(), label: "图鉴".to_string() },
                            Segment { value: "name".to_string(), label: "名称".to_string() },
                            Segment { value: "power".to_string(), label: "配种值".to_string() },
                            Segment { value: "added".to_string(), label: "最新".to_string() },
                        ],
                        value: sort,
                    }
                }
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
                            rsx! {
                                button {
                                    key: "{pal.id}",
                                    class: "owned-item",
                                    onclick: move |_| {
                                        editing.set(Some(p.clone()));
                                        dialog_open.set(true);
                                    },
                                    img { src: icon_url(&pal.species), alt: "{sp.name_zh}" }
                                    div { class: "owned-item-main",
                                        div { class: "owned-item-name",
                                            "{sp.name_zh}"
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
        ImportExportDialog { open: io_open }
    }
}

/// 导入 / 导出对话框。
#[component]
fn ImportExportDialog(open: Signal<bool>) -> Element {
    let store = use_context::<OwnedStore>();
    let mut pals = store.pals;
    let mut import_text = use_signal(String::new);
    let import_mode = use_signal(|| "append".to_string());
    let mut message = use_signal(|| None::<(bool, String)>);
    let mut copy_label = use_signal(|| "复制到剪贴板".to_string());

    let exported = if *open.read() {
        export_json(&pals.read())
    } else {
        String::new()
    };

    let exported_for_copy = exported.clone();
    let do_copy = move |_| {
        if copy_to_clipboard(&exported_for_copy) {
            copy_label.set("已复制 ✓".to_string());
            let cb = wasm_bindgen::closure::Closure::once(move || {
                copy_label.set("复制到剪贴板".to_string())
            });
            if let Some(w) = web_sys::window() {
                let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                    cb.as_ref().unchecked_ref(),
                    1500,
                );
            }
            cb.forget();
        } else {
            copy_label.set("复制失败，请手动全选复制".to_string());
        }
    };

    let do_import = move |_| {
        let text = import_text.read().clone();
        match parse_import(&text) {
            Ok(mut imported) => {
                let count = imported.len();
                let mut list = pals.write();
                if import_mode.read().as_str() == "replace" {
                    for (i, p) in imported.iter_mut().enumerate() {
                        p.id = (i + 1) as u64;
                    }
                    *list = imported;
                } else {
                    let mut next = list.iter().map(|p| p.id).max().unwrap_or(0) + 1;
                    for mut p in imported {
                        p.id = next;
                        next += 1;
                        list.push(p);
                    }
                }
                let mode_label = if import_mode.read().as_str() == "replace" {
                    "覆盖"
                } else {
                    "追加"
                };
                drop(list);
                message.set(Some((true, format!("已{mode_label}导入 {count} 只帕鲁"))));
                import_text.set(String::new());
            }
            Err(e) => message.set(Some((false, e))),
        }
    };

    rsx! {
        Dialog {
            open,
            title: "导入 / 导出".to_string(),
            description: "在不同设备或部署之间迁移你的帕鲁列表".to_string(),
            div { class: "form-row",
                label { class: "field-label", "导出（当前列表的 JSON）" }
                textarea {
                    class: "input textarea",
                    readonly: true,
                    value: "{exported}",
                    onclick: move |e| {
                        if let Some(el) = e.data().downcast::<web_sys::HtmlTextAreaElement>() {
                            el.select();
                        }
                    },
                }
                div { style: "margin-top: 8px; display: flex; justify-content: flex-end;",
                    Button { variant: BtnVariant::Outline, sm: true, onclick: do_copy, "{copy_label}" }
                }
            }
            div { class: "form-row",
                label { class: "field-label", "导入（粘贴 JSON）" }
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
                    for (i, mut s) in slots.into_iter().enumerate() {
                        s.set(p.passives.get(i).cloned());
                    }
                }
                None => {
                    species.set(None);
                    gender.set("male".to_string());
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
                    *entry = OwnedPal {
                        id: old.id,
                        species: sp,
                        gender: g,
                        passives,
                    };
                }
            }
            None => {
                let mut list = pals.write();
                let id = list.iter().map(|p| p.id).max().unwrap_or(0) + 1;
                list.push(OwnedPal {
                    id,
                    species: sp,
                    gender: g,
                    passives,
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
                label { class: "field-label", "性别" }
                Segmented {
                    options: vec![
                        Segment { value: "male".to_string(), label: "♂ 雄性".to_string() },
                        Segment { value: "female".to_string(), label: "♀ 雌性".to_string() },
                    ],
                    value: gender,
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
