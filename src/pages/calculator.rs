//! 配种查询页：正向（亲本→子代）与反向（子代→亲本组合）。

use crate::breeding::BreedOutcome;
use crate::ui::{ComboOption, Combobox};
use crate::{db, icon_url};
use dioxus::prelude::*;
use std::sync::OnceLock;

pub fn pal_options() -> &'static [ComboOption] {
    static OPTS: OnceLock<Vec<ComboOption>> = OnceLock::new();
    OPTS.get_or_init(|| {
        db().pals
            .iter()
            .map(|p| ComboOption {
                value: p.internal_name.clone(),
                label: p.name_zh.clone(),
                sublabel: Some(format!("#{} {}", p.paldex_no, p.name_en)),
                icon: Some(icon_url(&p.internal_name)),
            })
            .collect()
    })
}

#[component]
fn PalCardLg(internal_name: String, note: Option<String>) -> Element {
    let Some(p) = db().pal(&internal_name) else {
        return rsx! {};
    };
    rsx! {
        div { class: "pal-card-lg",
            img { src: icon_url(&p.internal_name), alt: "{p.name_zh}" }
            div {
                div { class: "name", "{p.name_zh}" }
                div { class: "sub", "{p.name_en} · 图鉴 #{p.paldex_no} · 配种值 {p.breeding_power}" }
                if let Some(note) = note {
                    div { class: "note", "{note}" }
                }
            }
        }
    }
}

/// 正向：选两只亲本，算子代。
#[component]
fn ForwardPanel() -> Element {
    let p1 = use_signal(|| None::<String>);
    let p2 = use_signal(|| None::<String>);

    let outcome = use_memo(move || {
        match (p1.read().clone(), p2.read().clone()) {
            (Some(a), Some(b)) => db().breed(&a, &b),
            _ => None,
        }
    });

    rsx! {
        div { class: "card",
            h3 { class: "card-title", "亲本 → 子代" }
            p { class: "card-desc", "选择两只亲本，计算配种结果" }
            div { class: "parents-row",
                Combobox { options: pal_options().to_vec(), value: p1, placeholder: "亲本 A：输入中文/英文名搜索" }
                div { class: "plus", "+" }
                Combobox { options: pal_options().to_vec(), value: p2, placeholder: "亲本 B" }
            }
            div { class: "result-zone",
                match &*outcome.read() {
                    Some(BreedOutcome::Normal(c)) => {
                        let child = db().pals[*c].internal_name.clone();
                        rsx! { PalCardLg { internal_name: child } }
                    }
                    Some(BreedOutcome::GenderDependent { if_p1_female, if_p2_female }) => {
                        let c1 = db().pals[*if_p1_female].internal_name.clone();
                        let c2 = db().pals[*if_p2_female].internal_name.clone();
                        rsx! {
                            div { class: "twins",
                                PalCardLg { internal_name: c1, note: "亲本 A 为雌性时".to_string() }
                                PalCardLg { internal_name: c2, note: "亲本 B 为雌性时".to_string() }
                            }
                        }
                    }
                    None => rsx! {
                        p { class: "hint", style: "padding-top: 16px;", "子代结果将显示在这里" }
                    },
                }
            }
        }
    }
}

/// 反向：选目标子代，列出全部亲本组合。
#[component]
fn ReversePanel() -> Element {
    let target = use_signal(|| None::<String>);

    let pairs = use_memo(move || {
        target
            .read()
            .clone()
            .map(|t| {
                let mut v = db().parents_of(&t);
                v.sort_by(|&(a, b, _), &(c, d, _)| {
                    let db = db();
                    (&db.pals[a].name_zh, &db.pals[b].name_zh)
                        .cmp(&(&db.pals[c].name_zh, &db.pals[d].name_zh))
                });
                v
            })
            .unwrap_or_default()
    });

    rsx! {
        div { class: "card",
            h3 { class: "card-title", "子代 → 亲本组合" }
            p { class: "card-desc", "选择目标帕鲁，列出所有能产出它的亲本组合" }
            Combobox { options: pal_options().to_vec(), value: target, placeholder: "目标帕鲁" }
            if target.read().is_some() {
                p { class: "hint", style: "margin: 12px 0 0;", "共 {pairs.read().len()} 种亲本组合" }
                ul { class: "pairs-grid",
                    for (a, b, outcome) in pairs.read().iter() {
                        {
                            let db = db();
                            let pa = db.pals[*a].clone();
                            let pb = db.pals[*b].clone();
                            let gender_note = matches!(outcome, BreedOutcome::GenderDependent { .. });
                            rsx! {
                                li { key: "{pa.internal_name}+{pb.internal_name}",
                                    img { src: icon_url(&pa.internal_name), alt: "" }
                                    span { "{pa.name_zh}" }
                                    span { class: "pair-plus", "+" }
                                    img { src: icon_url(&pb.internal_name), alt: "" }
                                    span { "{pb.name_zh}" }
                                    if gender_note {
                                        span { class: "note", "子代取决于雌性亲本" }
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

#[component]
pub fn Calculator() -> Element {
    rsx! {
        div { class: "page",
            h1 { class: "page-title", "配种查询" }
            p { class: "page-desc", "查询任意亲本组合的配种结果，或反查目标帕鲁的全部亲本组合" }
            ForwardPanel {}
            ReversePanel {}
        }
    }
}
