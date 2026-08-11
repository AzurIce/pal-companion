//! 数据生成工具：从 tylercamp/palcalc（MIT）拉取数据，推导唯一组合表与
//! 公式可产出集合，用穷举表 100% 回归校验后写出打包数据与图标。
//!
//! 用法：cargo run --bin gen_data --features gen-data

#[path = "../breeding.rs"]
mod breeding;

use breeding::{BreedOutcome, BreedingDB, Pal, Passive, UniqueCombo, formula_child};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::Path;

const DB_URL: &str =
    "https://raw.githubusercontent.com/tylercamp/palcalc/main/PalCalc.Model/db.json";
const DB_URL_MIRROR: &str =
    "https://cdn.jsdelivr.net/gh/tylercamp/palcalc@main/PalCalc.Model/db.json";
const BREEDING_URL: &str =
    "https://raw.githubusercontent.com/tylercamp/palcalc/main/PalCalc.Model/breeding.json";
const BREEDING_URL_MIRROR: &str =
    "https://cdn.jsdelivr.net/gh/tylercamp/palcalc@main/PalCalc.Model/breeding.json";
const ICON_URL_TEMPLATE: &str = "https://palworld.gg/images/full_palicon/T_{name}_icon_normal.png";

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawDb {
    pals: Vec<RawPal>,
    #[serde(default)]
    passive_skills: Vec<RawPassive>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawPassive {
    #[serde(default)]
    localized_names: Option<HashMap<String, String>>,
    internal_name: String,
    rank: i32,
    is_standard_passive_skill: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawPal {
    id: RawPalId,
    name: String,
    #[serde(default)]
    localized_names: Option<HashMap<String, String>>,
    internal_name: String,
    internal_index: i64,
    breeding_power: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawPalId {
    pal_dex_no: u32,
    is_variant: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawBreeding {
    breeding: Vec<RawBreed>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawBreed {
    parent1_internal_name: String,
    parent1_gender: String,
    parent2_internal_name: String,
    parent2_gender: String,
    child_internal_name: String,
}

fn fetch(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut resp = agent.get(url).call()?;
    let mut buf = Vec::new();
    resp.body_mut().as_reader().read_to_end(&mut buf)?;
    Ok(buf)
}

/// 主源失败时回退镜像源。
fn fetch_with_mirror(
    agent: &ureq::Agent,
    url: &str,
    mirror: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    match fetch(agent, url) {
        Ok(bytes) => Ok(bytes),
        Err(e) => {
            eprintln!("主源失败（{e}），尝试镜像源 ...");
            Ok(fetch(agent, mirror)?)
        }
    }
}

fn is_female(g: &str) -> bool {
    g.eq_ignore_ascii_case("female")
}

fn is_wildcard(g: &str) -> bool {
    g.eq_ignore_ascii_case("wildcard")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .user_agent("pal-companion gen_data (github.com/tylercamp/palcalc data fetch)")
        .timeout_global(Some(std::time::Duration::from_secs(120)))
        .build()
        .into();

    println!("下载 db.json ...");
    let db_bytes = fetch_with_mirror(&agent, DB_URL, DB_URL_MIRROR)?;
    println!("下载 breeding.json ...");
    let breeding_bytes = fetch_with_mirror(&agent, BREEDING_URL, BREEDING_URL_MIRROR)?;

    let raw_db: RawDb = serde_json::from_slice(&db_bytes)?;
    let raw_breeds: Vec<RawBreed> = serde_json::from_slice::<RawBreeding>(&breeding_bytes)?.breeding;
    println!(
        "帕鲁 {} 只，配种记录 {} 条",
        raw_db.pals.len(),
        raw_breeds.len()
    );

    // 1. 裁剪帕鲁与被动技能数据
    let RawDb {
        pals: raw_pals,
        passive_skills,
    } = raw_db;
    let mut passives: Vec<Passive> = passive_skills
        .into_iter()
        .filter(|p| p.is_standard_passive_skill)
        .map(|p| {
            let name_en = p
                .localized_names
                .as_ref()
                .and_then(|m| m.get("en"))
                .cloned()
                .unwrap_or_else(|| p.internal_name.clone());
            let name_zh = p
                .localized_names
                .as_ref()
                .and_then(|m| m.get("zh-Hans"))
                .cloned()
                .unwrap_or_else(|| name_en.clone());
            Passive {
                internal_name: p.internal_name,
                name_zh,
                name_en,
                rank: p.rank,
            }
        })
        .collect();
    passives.sort_by_key(|p| (-p.rank, p.internal_name.clone()));
    let mut pals: Vec<Pal> = raw_pals
        .into_iter()
        .map(|p| {
            let name_en = p
                .localized_names
                .as_ref()
                .and_then(|m| m.get("en"))
                .cloned()
                .unwrap_or_else(|| p.name.clone());
            let name_zh = p
                .localized_names
                .as_ref()
                .and_then(|m| m.get("zh-Hans"))
                .cloned()
                .unwrap_or_else(|| name_en.clone());
            Pal {
                internal_name: p.internal_name,
                paldex_no: p.id.pal_dex_no,
                is_variant: p.id.is_variant,
                name_zh,
                name_en,
                breeding_power: p.breeding_power,
                internal_index: p.internal_index,
                breedable_child: false, // 推导后回填
            }
        })
        .collect();
    pals.sort_by_key(|p| p.internal_index);
    let name_set: BTreeSet<&str> = pals.iter().map(|p| p.internal_name.as_str()).collect();

    // 2. 分类配种记录
    let mut same_species = 0usize;
    // (pair) -> {female_parent -> child}
    let mut gender_combos: BTreeMap<(String, String), BTreeMap<String, String>> = BTreeMap::new();
    // 无序 pair -> child（不同种、无性别要求）
    let mut plain: BTreeMap<(String, String), String> = BTreeMap::new();

    for r in &raw_breeds {
        for n in [
            &r.parent1_internal_name,
            &r.parent2_internal_name,
            &r.child_internal_name,
        ] {
            if !name_set.contains(n.as_str()) {
                return Err(format!("配种记录引用了未知帕鲁: {n}").into());
            }
        }
        let (lo, hi) = ordered(&r.parent1_internal_name, &r.parent2_internal_name);
        if r.parent1_internal_name == r.parent2_internal_name {
            if r.child_internal_name != r.parent1_internal_name {
                return Err(format!("同种配种结果异常: {lo} -> {}", r.child_internal_name).into());
            }
            same_species += 1;
            continue;
        }
        if !is_wildcard(&r.parent1_gender) || !is_wildcard(&r.parent2_gender) {
            // 性别规则：雌性亲本决定子代
            let female = if is_female(&r.parent1_gender) {
                &r.parent1_internal_name
            } else if is_female(&r.parent2_gender) {
                &r.parent2_internal_name
            } else {
                return Err(format!("无法识别的性别规则记录: {lo} + {hi}").into());
            };
            let entry = gender_combos.entry((lo.clone(), hi.clone())).or_default();
            if let Some(prev) = entry.insert(female.clone(), r.child_internal_name.clone()) {
                if prev != r.child_internal_name {
                    return Err(format!("性别组合冲突: {lo} + {hi}（雌性 {female}）").into());
                }
            }
            continue;
        }
        if let Some(prev) = plain.insert((lo.clone(), hi.clone()), r.child_internal_name.clone()) {
            if prev != r.child_internal_name {
                return Err(format!("配对结果冲突: {lo} + {hi}").into());
            }
        }
    }
    println!(
        "同种记录 {same_species} 条，性别组合 {} 对，普通配对 {} 对",
        gender_combos.len(),
        plain.len()
    );

    // 3. 迭代推导 eligible（公式可产出）集合到不动点：
    //    初始为"普通配对中出现过为子代"的超集（会被唯一组合子代污染，如传说变体）；
    //    每轮用公式重算所有普通配对，eligible 收缩为真实命中的子代集合；
    //    集合不再变化时，仍不匹配的配对即唯一组合。
    let by_name: HashMap<String, usize> = pals
        .iter()
        .enumerate()
        .map(|(i, p)| (p.internal_name.clone(), i))
        .collect();
    let superset_children: BTreeSet<&str> = plain.values().map(|c| c.as_str()).collect();
    let mut eligible: Vec<usize> = pals
        .iter()
        .enumerate()
        .filter(|(_, p)| superset_children.contains(p.internal_name.as_str()))
        .map(|(i, _)| i)
        .collect();

    let mut unique_combos: Vec<UniqueCombo> = Vec::new();
    loop {
        let mut matched_children: BTreeSet<usize> = BTreeSet::new();
        let mut specials: Vec<UniqueCombo> = Vec::new();
        for ((lo, hi), child) in &plain {
            let a = &pals[by_name[lo.as_str()]];
            let b = &pals[by_name[hi.as_str()]];
            let f = formula_child(&pals, &eligible, a, b);
            if pals[f].internal_name == *child {
                matched_children.insert(f);
            } else {
                specials.push(UniqueCombo {
                    parent1: lo.clone(),
                    parent2: hi.clone(),
                    child: child.clone(),
                    female_parent: None,
                });
            }
        }
        let new_eligible: Vec<usize> = eligible
            .iter()
            .copied()
            .filter(|i| matched_children.contains(i))
            .collect();
        if new_eligible.len() == eligible.len() {
            unique_combos = specials;
            break;
        }
        eligible = new_eligible;
    }
    for p in pals.iter_mut() {
        p.breedable_child = eligible.contains(&by_name[p.internal_name.as_str()]);
    }

    // 性别组合写入唯一组合表
    for ((lo, hi), by_female) in &gender_combos {
        for (female, child) in by_female {
            unique_combos.push(UniqueCombo {
                parent1: lo.clone(),
                parent2: hi.clone(),
                child: child.clone(),
                female_parent: Some(female.clone()),
            });
        }
    }
    unique_combos.sort_by(|a, b| {
        (&a.parent1, &a.parent2, &a.child).cmp(&(&b.parent1, &b.parent2, &b.child))
    });
    let eligible_count = pals.iter().filter(|p| p.breedable_child).count();
    println!(
        "推导：公式可产出 {} 种，唯一组合 {} 条（含性别规则）",
        eligible_count,
        unique_combos.len()
    );

    // 4. 用全部穷举记录回归校验最终逻辑
    let db = BreedingDB::new(pals.clone(), unique_combos.clone());
    let mut mismatches = 0usize;
    for r in &raw_breeds {
        let outcome = db
            .breed(&r.parent1_internal_name, &r.parent2_internal_name)
            .expect("breed 不应失败");
        let ok = match outcome {
            BreedOutcome::Normal(c) => {
                is_wildcard(&r.parent1_gender)
                    && is_wildcard(&r.parent2_gender)
                    && db.pals[c].internal_name == r.child_internal_name
            }
            BreedOutcome::GenderDependent {
                if_p1_female,
                if_p2_female,
            } => {
                let expected = if is_female(&r.parent1_gender) {
                    &db.pals[if_p1_female]
                } else if is_female(&r.parent2_gender) {
                    &db.pals[if_p2_female]
                } else {
                    mismatches += 1;
                    eprintln!("校验失败（性别）: {r:?}");
                    continue;
                };
                expected.internal_name == r.child_internal_name
            }
        };
        if !ok {
            mismatches += 1;
            if mismatches <= 20 {
                let actual = match outcome {
                    BreedOutcome::Normal(c) => db.pals[c].internal_name.clone(),
                    BreedOutcome::GenderDependent {
                        if_p1_female,
                        if_p2_female,
                    } => format!(
                        "{}(p1♀)/{}(p2♀)",
                        db.pals[if_p1_female].internal_name, db.pals[if_p2_female].internal_name
                    ),
                };
                eprintln!(
                    "校验失败: {} + {} => 期望 {}, 实际 {}",
                    r.parent1_internal_name, r.parent2_internal_name, r.child_internal_name, actual
                );
            }
        }
    }
    if mismatches > 0 {
        return Err(format!("回归校验失败：{mismatches} 条记录不匹配").into());
    }
    println!("回归校验通过：{} 条记录 100% 匹配", raw_breeds.len());

    // 已知用例抽查
    for (p1, p2, expect) in [
        ("HadesBird", "IceHorse", "IceHorse_Dark"), // Helzephyr + Frostallion → Frostallion Noct
        ("IceFox", "Kitsunebi", "Kitsunebi_Ice"),   // Foxcicle + Foxparks → Foxparks Cryst
    ] {
        match db.breed(p1, p2) {
            Some(BreedOutcome::Normal(c)) if db.pals[c].internal_name == expect => {}
            other => eprintln!("警告：已知用例 {p1} + {p2} 结果异常: {other:?}（内部名可能已变）"),
        }
    }

    // 5. 写出数据文件
    std::fs::create_dir_all("assets/data")?;
    std::fs::write("assets/data/pals.json", serde_json::to_string(&pals)?)?;
    std::fs::write(
        "assets/data/unique_combos.json",
        serde_json::to_string(&unique_combos)?,
    )?;
    std::fs::write(
        "assets/data/passives.json",
        serde_json::to_string(&passives)?,
    )?;
    println!(
        "已写出 assets/data/pals.json、unique_combos.json、passives.json（{} 个被动）",
        passives.len()
    );

    // 6. 下载图标（跳过已存在）
    std::fs::create_dir_all("public/icons")?;
    let mut failed = Vec::new();
    for p in &pals {
        let path = format!("public/icons/{}.png", p.internal_name);
        if Path::new(&path).exists() {
            continue;
        }
        let url = ICON_URL_TEMPLATE.replace("{name}", &p.internal_name);
        match fetch(&agent, &url) {
            Ok(bytes) if bytes.len() > 100 => std::fs::write(&path, bytes)?,
            // 变体图标缺失时回退用基础物种的图标
            _ => match p.internal_name.split_once('_') {
                Some((base, _)) => {
                    let base_url = ICON_URL_TEMPLATE.replace("{name}", base);
                    match fetch(&agent, &base_url) {
                        Ok(bytes) if bytes.len() > 100 => std::fs::write(&path, bytes)?,
                        _ => failed.push(p.internal_name.clone()),
                    }
                }
                None => failed.push(p.internal_name.clone()),
            },
        }
    }
    if failed.is_empty() {
        println!("图标全部就绪（{} 只）", pals.len());
    } else {
        eprintln!("警告：{} 个图标下载失败: {:?}", failed.len(), failed);
    }
    Ok(())
}

fn ordered(a: &str, b: &str) -> (String, String) {
    if a <= b { (a.into(), b.into()) } else { (b.into(), a.into()) }
}
