//! 用构建期生成的真实数据做已知用例回归（需先运行 `cargo run --bin gen_data --features gen-data`）。

#[path = "../src/breeding.rs"]
mod breeding;
#[path = "../src/planner.rs"]
mod planner;

use breeding::{BreedOutcome, BreedingDB};
use planner::{Gender, OwnedPal, PlanSource};
use std::collections::HashMap;

fn load_db() -> BreedingDB {
    breeding::from_json(
        &std::fs::read_to_string("assets/data/pals.json").expect("缺少 assets/data/pals.json，请先运行 cargo run --bin gen_data --features gen-data"),
        &std::fs::read_to_string("assets/data/unique_combos.json")
            .expect("缺少 assets/data/unique_combos.json，请先运行 cargo run --bin gen_data --features gen-data"),
    )
    .unwrap()
}

fn normal_child(db: &BreedingDB, p1: &str, p2: &str) -> String {
    match db.breed(p1, p2) {
        Some(BreedOutcome::Normal(c)) => db.pals[c].internal_name.clone(),
        other => panic!("{p1} + {p2} 结果异常: {other:?}"),
    }
}

#[test]
fn known_unique_combos() {
    let db = load_db();
    // Helzephyr + Frostallion → Frostallion Noct
    assert_eq!(normal_child(&db, "HadesBird", "IceHorse"), "IceHorse_Dark");
    // Foxcicle + Foxparks → Foxparks Cryst
    assert_eq!(normal_child(&db, "IceFox", "Kitsunebi"), "Kitsunebi_Ice");
    // 与参数顺序无关
    assert_eq!(normal_child(&db, "Kitsunebi", "IceFox"), "Kitsunebi_Ice");
}

#[test]
fn known_gender_combo() {
    let db = load_db();
    // Katress + Wixen：雌性决定子代（Katress♀ → Katress Ignis；Wixen♀ → Wixen Noct）
    match db.breed("FoxMage", "CatMage") {
        Some(BreedOutcome::GenderDependent {
            if_p1_female,
            if_p2_female,
        }) => {
            assert_eq!(db.pals[if_p1_female].internal_name, "FoxMage_Dark");
            assert_eq!(db.pals[if_p2_female].internal_name, "CatMage_Fire");
        }
        other => panic!("性别组合结果异常: {other:?}"),
    }
}

#[test]
fn same_species_and_reverse_lookup() {
    let db = load_db();
    assert_eq!(normal_child(&db, "Anubis", "Anubis"), "Anubis");
    // Anubis 的同种自配应在反向查询结果中
    let parents = db.parents_of("Anubis");
    let anubis = db.index_of("Anubis").unwrap();
    assert!(parents.iter().any(|&(a, b, _)| a == anubis && b == anubis));
    assert!(!parents.is_empty());
}

fn op(id: u64, species: &str, gender: Gender) -> OwnedPal {
    OwnedPal {
        id,
        species: species.into(),
        gender,
        passives: vec![],
    }
}

/// 真实数据：钉选每个备选组合，根节点亲本对必须随之改变（局部替换，不跑搜索约束）。
#[test]
fn pin_alternative_takes_effect_with_real_data() {
    let db = load_db();
    // 沁莲龙（LotusDragon）：灵曦龙×腾炎龙 或 墨罗娜×腾炎龙
    let owned = vec![
        op(1, "GhostDragon", Gender::Female),
        op(2, "Umihebi_Fire", Gender::Male),
        op(3, "MonochromeQueen", Gender::Female),
    ];
    let base = planner::plan(&db, &owned, "LotusDragon", &[]).unwrap();
    let alts = &base.alternatives["LotusDragon"];
    assert!(alts.len() >= 2, "沁莲龙应至少有 2 个备选组合: {alts:?}");
    for alt in alts {
        let mut pins = HashMap::new();
        pins.insert("LotusDragon".to_string(), alt.parents.clone());
        let p = planner::plan_with_pins(&db, &owned, "LotusDragon", &[], &pins)
            .expect("钉选备选组合不应导致不可达");
        let PlanSource::Bred { p1, p2, .. } = &p.root.source else {
            panic!("应为配种节点");
        };
        let mut got = [p1.species.clone(), p2.species.clone()];
        got.sort();
        assert_eq!(
            (got[0].as_str(), got[1].as_str()),
            (alt.parents.0.as_str(), alt.parents.1.as_str()),
            "钉选 {:?} 未生效",
            alt.parents
        );
    }
}
