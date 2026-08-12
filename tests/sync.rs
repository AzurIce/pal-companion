//! WebSocket 同步的纯逻辑回归：消息解析（版本/字段容错）与去重合并。

#[path = "../src/breeding.rs"]
mod breeding;
#[path = "../src/planner.rs"]
mod planner;
#[path = "../src/sync.rs"]
mod sync;

use planner::{Gender, OwnedPal, PalGroup};
use sync::{SyncError, SyncedPal};

fn owned(id: u64, species: &str, gender: Gender, passives: &[&str]) -> OwnedPal {
    OwnedPal {
        id,
        species: species.to_string(),
        gender,
        passives: passives.iter().map(|s| s.to_string()).collect(),
        group: PalGroup::Box,
        is_boss: false,
        favorite: 0,
        nickname: None,
        is_lucky: false,
        basecamp: None,
    }
}

#[test]
fn parse_valid_message() {
    let text = r#"{"version":1,"source":"palws","pals":[
        {"species":"PinkCat","gender":"male","passives":["Brave"],"nickname":"喵喵","level":12},
        {"species":"SheepBall","gender":"female"}
    ]}"#;
    let pals = sync::parse_message(text).unwrap();
    assert_eq!(pals.len(), 2);
    assert_eq!(
        pals[0],
        SyncedPal {
            species: Some("PinkCat".to_string()),
            gender: "male".to_string(),
            passives: vec!["Brave".to_string()],
            nickname: Some("喵喵".to_string()),
            level: Some(12),
            ..Default::default()
        }
    );
    // 可选字段缺省
    assert_eq!(pals[1].passives, Vec::<String>::new());
    assert_eq!(pals[1].nickname, None);
    assert_eq!(pals[1].level, None);
}

#[test]
fn parse_null_fields() {
    // mod 真实输出：passives / nickname 可能是 null
    let text = r#"{"version":1,"source":"palws","pals":[
        {"species":"Sheepball","gender":"unknown","passives":null,"nickname":null,"level":2},
        {"species":null,"gender":"male","passives":["Brave"],"nickname":null,"level":null},
        {"species":"Foxparks","gender":null,"passives":["Brave"]}
    ]}"#;
    let pals = sync::parse_message(text).unwrap();
    assert_eq!(pals.len(), 3);
    // passives null → 空数组
    assert_eq!(pals[0].passives, Vec::<String>::new());
    assert_eq!(pals[0].nickname, None);
    // species / gender null → None / 空串（后续按"跳过该只"处理）
    assert_eq!(pals[1].species, None);
    assert_eq!(pals[2].gender, "");
}

#[test]
fn parse_skips_bad_entries() {
    // 单只字段类型完全对不上（如 level 是字符串）只丢该只，不影响其他
    let text = r#"{"version":1,"source":"palws","pals":[
        {"species":"PinkCat","gender":"male","passives":[],"level":"oops"},
        {"species":"SheepBall","gender":"female","passives":[]},
        "garbage"
    ]}"#;
    let pals = sync::parse_message(text).unwrap();
    assert_eq!(pals.len(), 1);
    assert_eq!(pals[0].species, Some("SheepBall".to_string()));
}

#[test]
fn draft_skips_missing_species_and_unknown_gender() {
    let ok = SyncedPal {
        species: Some("PinkCat".to_string()),
        gender: "female".to_string(),
        passives: vec!["Brave".to_string()],
        nickname: None,
        level: None,
        ..Default::default()
    };
    assert_eq!(
        ok.to_owned_draft(),
        Some(OwnedPal {
            id: 0,
            species: "PinkCat".to_string(),
            gender: Gender::Female,
            passives: vec!["Brave".to_string()],
            group: PalGroup::Box,
            is_boss: false,
            favorite: 0,
            nickname: None,
            is_lucky: false,
            basecamp: None,
        })
    );
    // species 为 null → 跳过
    let no_species = SyncedPal {
        species: None,
        ..ok.clone()
    };
    assert_eq!(no_species.to_owned_draft(), None);
    // gender unknown → 跳过
    let unknown_gender = SyncedPal {
        gender: "unknown".to_string(),
        ..ok.clone()
    };
    assert_eq!(unknown_gender.to_owned_draft(), None);
}

#[test]
fn garbage_passives_pass_through_parse() {
    // mod 侧在修的垃圾字符串（如 "RemoteUnrealParam: ..."）解析层原样保留；
    // 剔除由 sync_client 合并时按内置被动表完成（passive_by_internal）。
    let text = r#"{"version":1,"pals":[
        {"species":"Sheepball","gender":"male","passives":["RemoteUnrealParam: 00000233CA3CFB08"]}
    ]}"#;
    let pals = sync::parse_message(text).unwrap();
    assert_eq!(
        pals[0].passives,
        vec!["RemoteUnrealParam: 00000233CA3CFB08".to_string()]
    );
}

#[test]
fn parse_version_mismatch() {
    let text = r#"{"version":2,"source":"palws","pals":[]}"#;
    assert_eq!(
        sync::parse_message(text),
        Err(SyncError::UnsupportedVersion(2))
    );
}

#[test]
fn parse_invalid_json() {
    assert!(matches!(
        sync::parse_message("not json"),
        Err(SyncError::Parse(_))
    ));
}

#[test]
fn parse_gender_mapping() {
    assert_eq!(sync::parse_gender("male"), Some(Gender::Male));
    assert_eq!(sync::parse_gender("female"), Some(Gender::Female));
    assert_eq!(sync::parse_gender("unknown"), None);
    assert_eq!(sync::parse_gender(""), None);
}

#[test]
fn merge_dedupes_against_existing() {
    let mut existing = vec![owned(1, "PinkCat", Gender::Male, &["Brave"])];
    let incoming = vec![
        // 与现有完全相同（被动顺序不同也算同一只）→ 跳过
        owned(0, "PinkCat", Gender::Male, &["Brave"]),
        // 性别不同 → 新增
        owned(0, "PinkCat", Gender::Female, &["Brave"]),
        // 被动不同 → 新增
        owned(0, "PinkCat", Gender::Male, &["Brave", "Lucky"]),
    ];
    let added = sync::merge_owned(&mut existing, incoming);
    assert_eq!(added, 2);
    assert_eq!(existing.len(), 3);
    // id 按 max+1 递增
    assert_eq!(existing[1].id, 2);
    assert_eq!(existing[2].id, 3);
}

#[test]
fn merge_passive_order_insensitive() {
    let mut existing = vec![owned(7, "Anubis", Gender::Male, &["A", "B"])];
    let incoming = vec![owned(0, "Anubis", Gender::Male, &["B", "A"])];
    assert_eq!(sync::merge_owned(&mut existing, incoming), 0);
    assert_eq!(existing.len(), 1);
}

#[test]
fn merge_dedupes_within_incoming() {
    let mut existing = Vec::new();
    let incoming = vec![
        owned(0, "Foxparks", Gender::Male, &[]),
        owned(0, "Foxparks", Gender::Male, &[]),
    ];
    assert_eq!(sync::merge_owned(&mut existing, incoming), 1);
    assert_eq!(existing[0].id, 1);
}

#[test]
fn strip_boss_prefix_variants() {
    assert_eq!(sync::strip_boss_prefix("BOSS_SheepBall"), "SheepBall");
    assert_eq!(sync::strip_boss_prefix("Boss_Anubis"), "Anubis");
    assert_eq!(sync::strip_boss_prefix("boss_Foxparks"), "Foxparks");
    // 无前缀 / 空前缀体 / 仅前缀的情况原样返回
    assert_eq!(sync::strip_boss_prefix("SheepBall"), "SheepBall");
    assert_eq!(sync::strip_boss_prefix("BOSS_"), "BOSS_");
    assert_eq!(sync::strip_boss_prefix("BOSSY_Cat"), "BOSSY_Cat");
}

#[test]
fn parse_group_mapping() {
    use planner::PalGroup;
    assert_eq!(sync::parse_group(Some("party")), PalGroup::Party);
    assert_eq!(sync::parse_group(Some("box")), PalGroup::Box);
    assert_eq!(sync::parse_group(Some("base")), PalGroup::Base);
    assert_eq!(sync::parse_group(Some("???")), PalGroup::Box);
    assert_eq!(sync::parse_group(None), PalGroup::Box);
}

#[test]
fn merge_refreshes_group_on_resync() {
    use planner::PalGroup;
    let mut existing = vec![owned(1, "Foxparks", Gender::Male, &[])];
    // 同一只帕鲁再次同步（位置从盒子变为队伍）：不新增，只刷新 group
    let mut moved = owned(0, "Foxparks", Gender::Male, &[]);
    moved.group = PalGroup::Party;
    assert_eq!(sync::merge_owned(&mut existing, vec![moved]), 0);
    assert_eq!(existing.len(), 1);
    assert_eq!(existing[0].group, PalGroup::Party);
}
