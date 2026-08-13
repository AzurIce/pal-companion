//! WebSocket 同步的纯逻辑回归：envelope 解析（版本/协议/字段容错）与合并语义。

#[path = "../src/breeding.rs"]
mod breeding;
#[path = "../src/planner.rs"]
mod planner;
#[path = "../src/sync.rs"]
mod sync;

use planner::{Gender, OwnedPal, PalGroup};
use sync::{ServerEvent, SnapshotStats, SyncError, SyncedPal};

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
        synced: false,
    }
}

// ---------------------------------------------------------------------------
// envelope 解析
// ---------------------------------------------------------------------------

#[test]
fn parse_hello() {
    let text = r#"{"protocol":"palws","version":1,"type":"server.hello","id":"srv-1","seq":1,
        "payload":{"server_version":"palws-0.1.0","capabilities":["snapshot","snapshot.request"],
        "clients":1,"sync_state":"idle"}}"#;
    let ev = sync::parse_server_message(text).unwrap();
    assert_eq!(
        ev,
        ServerEvent::Hello {
            server_version: "palws-0.1.0".to_string(),
            capabilities: vec!["snapshot".to_string(), "snapshot.request".to_string()],
            clients: 1,
            sync_state: "idle".to_string(),
        }
    );
}

#[test]
fn parse_sync_status() {
    let text = r#"{"protocol":"palws","version":1,"type":"sync.status","id":"lua-1","request_id":"req-7","seq":2,
        "payload":{"phase":"requesting","requested_pages":12,"total_pages":32,"trigger":"web"}}"#;
    let ev = sync::parse_server_message(text).unwrap();
    assert_eq!(
        ev,
        ServerEvent::SyncStatus {
            request_id: Some("req-7".to_string()),
            phase: "requesting".to_string(),
            requested_pages: 12,
            total_pages: 32,
            trigger: "web".to_string(),
        }
    );
}

#[test]
fn parse_snapshot() {
    let text = r#"{"protocol":"palws","version":1,"type":"snapshot","id":"lua-2","request_id":"req-7","seq":9,
        "payload":{"mode":"replace","pals":[
            {"species":"PinkCat","gender":"male","passives":["Brave"],"nickname":"喵喵","level":12},
            {"species":"SheepBall","gender":"female"},
            {"species":"Foxparks","gender":"male","passives":null,"level":"oops"}
        ],"stats":{"total":2,"requested_pages":32,"request_errors":0,"containers":5}}}"#;
    let ev = sync::parse_server_message(text).unwrap();
    match ev {
        ServerEvent::Snapshot {
            request_id,
            seq,
            mode,
            pals,
            stats,
        } => {
            assert_eq!(request_id, Some("req-7".to_string()));
            assert_eq!(seq, 9);
            assert_eq!(mode, "replace");
            // 坏条目（level 为字符串）被跳过
            assert_eq!(pals.len(), 2);
            assert_eq!(pals[0].species, Some("PinkCat".to_string()));
            assert_eq!(
                stats,
                SnapshotStats {
                    total: 2,
                    requested_pages: 32,
                    request_errors: 0,
                    containers: 5,
                }
            );
        }
        _ => panic!("expected snapshot"),
    }
}

#[test]
fn parse_log_error_pong() {
    let log = r#"{"protocol":"palws","version":1,"type":"log","id":"lua-1","request_id":"req-7",
        "payload":{"level":"warn","source":"lua","message":"page 3 failed"}}"#;
    assert_eq!(
        sync::parse_server_message(log).unwrap(),
        ServerEvent::Log {
            request_id: Some("req-7".to_string()),
            level: "warn".to_string(),
            source: "lua".to_string(),
            message: "page 3 failed".to_string(),
        }
    );

    let err = r#"{"protocol":"palws","version":1,"type":"error","id":"srv-2","request_id":"req-7",
        "payload":{"code":"player-state-unavailable","message":"当前未进入可同步的游戏世界","retryable":true}}"#;
    assert_eq!(
        sync::parse_server_message(err).unwrap(),
        ServerEvent::Error {
            request_id: Some("req-7".to_string()),
            code: "player-state-unavailable".to_string(),
            message: "当前未进入可同步的游戏世界".to_string(),
            retryable: true,
        }
    );

    let pong = r#"{"protocol":"palws","version":1,"type":"pong","id":"srv-3",
        "payload":{"echo_id":"ping-1"}}"#;
    assert_eq!(
        sync::parse_server_message(pong).unwrap(),
        ServerEvent::Pong {
            echo_id: "ping-1".to_string(),
        }
    );
}

#[test]
fn parse_unknown_type() {
    let text = r#"{"protocol":"palws","version":1,"type":"future-thing","payload":{}}"#;
    assert_eq!(
        sync::parse_server_message(text).unwrap(),
        ServerEvent::Unknown {
            mtype: "future-thing".to_string()
        }
    );
}

#[test]
fn parse_version_mismatch() {
    let text = r#"{"protocol":"palws","version":2,"type":"snapshot","payload":{"pals":[]}}"#;
    assert_eq!(
        sync::parse_server_message(text),
        Err(SyncError::UnsupportedVersion(2))
    );
}

#[test]
fn parse_protocol_mismatch() {
    let text = r#"{"protocol":"other","version":1,"type":"snapshot","payload":{"pals":[]}}"#;
    assert_eq!(
        sync::parse_server_message(text),
        Err(SyncError::UnsupportedProtocol("other".to_string()))
    );
}

#[test]
fn parse_invalid_json() {
    assert!(matches!(
        sync::parse_server_message("not json"),
        Err(SyncError::Parse(_))
    ));
}

// ---------------------------------------------------------------------------
// 客户端请求构造
// ---------------------------------------------------------------------------

#[test]
fn client_request_builders() {
    assert!(sync::client_hello_json("0.1.0").contains("\"type\":\"client.hello\""));
    assert!(sync::snapshot_request_json("req-1").contains("\"type\":\"snapshot.request\""));
    assert!(sync::ping_json("ping-1").contains("\"type\":\"ping\""));
}

// ---------------------------------------------------------------------------
// SyncedPal 转草稿 / 字段映射
// ---------------------------------------------------------------------------

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
            synced: true,
        })
    );
    let no_species = SyncedPal {
        species: None,
        ..ok.clone()
    };
    assert_eq!(no_species.to_owned_draft(), None);
    let unknown_gender = SyncedPal {
        gender: "unknown".to_string(),
        ..ok.clone()
    };
    assert_eq!(unknown_gender.to_owned_draft(), None);
}

#[test]
fn parse_gender_mapping() {
    assert_eq!(sync::parse_gender("male"), Some(Gender::Male));
    assert_eq!(sync::parse_gender("female"), Some(Gender::Female));
    assert_eq!(sync::parse_gender("unknown"), None);
    assert_eq!(sync::parse_gender(""), None);
}

#[test]
fn parse_group_mapping() {
    assert_eq!(sync::parse_group(Some("party")), PalGroup::Party);
    assert_eq!(sync::parse_group(Some("box")), PalGroup::Box);
    assert_eq!(sync::parse_group(Some("base")), PalGroup::Base);
    assert_eq!(sync::parse_group(Some("???")), PalGroup::Box);
    assert_eq!(sync::parse_group(None), PalGroup::Box);
}

#[test]
fn strip_boss_prefix_variants() {
    assert_eq!(sync::strip_boss_prefix("BOSS_SheepBall"), "SheepBall");
    assert_eq!(sync::strip_boss_prefix("Boss_Anubis"), "Anubis");
    assert_eq!(sync::strip_boss_prefix("boss_Foxparks"), "Foxparks");
    assert_eq!(sync::strip_boss_prefix("SheepBall"), "SheepBall");
    assert_eq!(sync::strip_boss_prefix("BOSS_"), "BOSS_");
    assert_eq!(sync::strip_boss_prefix("BOSSY_Cat"), "BOSSY_Cat");
}

// ---------------------------------------------------------------------------
// 合并语义
// ---------------------------------------------------------------------------

#[test]
fn replace_synced_keeps_manual_entries() {
    // v1 推荐语义：同步条目全部替换，手工条目保留
    let mut manual = owned(1, "Foxparks", Gender::Male, &[]);
    manual.synced = false;
    let mut synced_old = owned(2, "Lamball", Gender::Female, &[]);
    synced_old.synced = true;
    let mut existing = vec![manual, synced_old];

    let incoming = vec![
        owned(0, "Chikipi", Gender::Male, &[]),
        owned(0, "Pengullet", Gender::Female, &[]),
    ];
    let n = sync::replace_synced(&mut existing, incoming);
    assert_eq!(n, 2);
    assert_eq!(existing.len(), 3, "手工条目保留 + 2 条新同步");
    // 手工条目仍在
    assert!(existing.iter().any(|p| p.species == "Foxparks" && !p.synced));
    // 旧的同步条目被替换
    assert!(!existing.iter().any(|p| p.species == "Lamball"));
    // 新同步条目 id 从手工最大 id 之后递增，且标记 synced
    let synced: Vec<&OwnedPal> = existing.iter().filter(|p| p.synced).collect();
    assert_eq!(synced.len(), 2);
    assert!(synced.iter().all(|p| p.id >= 2));
}

#[test]
fn replace_synced_empty_incoming_keeps_manual() {
    let mut manual = owned(5, "Foxparks", Gender::Male, &[]);
    manual.synced = false;
    let mut existing = vec![manual];
    assert_eq!(sync::replace_synced(&mut existing, Vec::new()), 0);
    assert_eq!(existing.len(), 1);
    assert_eq!(existing[0].species, "Foxparks");
}
