//! 用构建期生成的真实数据做已知用例回归（需先运行 `cargo run --bin gen_data --features gen-data`）。

#[path = "../src/breeding.rs"]
mod breeding;

use breeding::{BreedOutcome, BreedingDB};

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
