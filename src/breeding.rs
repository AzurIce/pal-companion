//! 幻兽帕鲁配种逻辑（纯函数，主 App 与 gen_data 共用）。
//!
//! 规则（与游戏数据一致，由 gen_data 用穷举表 100% 回归校验）：
//! 1. 唯一组合（`unique_combos.json`）优先于一切；
//! 2. 同种配种必出同种；
//! 3. 公式：target = (rankA + rankB + 1) / 2（实数，不取整），
//!    在 `breedable_child == true` 的帕鲁中取 |rank - target| 最小者，
//!    平局取 `internal_index` 最小者（游戏文件行序）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pal {
    pub internal_name: String,
    pub paldex_no: u32,
    pub is_variant: bool,
    pub name_zh: String,
    pub name_en: String,
    pub breeding_power: i64,
    pub internal_index: i64,
    /// 是否可以作为公式的结果产出（变体/传说为 false）
    pub breedable_child: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UniqueCombo {
    /// 无序配对（parent1 <= parent2 字典序）
    pub parent1: String,
    pub parent2: String,
    pub child: String,
    /// 性别规则：Some(p) 表示当 p（parent1 或 parent2 之一）为雌性时产出 child。
    /// None 表示与性别无关。
    #[serde(default)]
    pub female_parent: Option<String>,
}

/// 被动技能（特性）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Passive {
    pub internal_name: String,
    pub name_zh: String,
    pub name_en: String,
    /// 稀有度层级：正值为正面（越高越强），负值为负面
    pub rank: i32,
    /// 效果描述
    pub desc_zh: String,
    pub desc_en: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreedOutcome {
    /// 确定结果：子代在 pals 中的索引
    Normal(usize),
    /// 性别相关组合：分别给出 parent1 / parent2 为雌性时的子代
    GenderDependent {
        if_p1_female: usize,
        if_p2_female: usize,
    },
}

pub struct BreedingDB {
    pub pals: Vec<Pal>,
    pub unique_combos: Vec<UniqueCombo>,
    by_name: HashMap<String, usize>,
    /// (parent1, parent2) 有序对 -> unique_combos 索引
    combo_index: HashMap<(String, String), Vec<usize>>,
    /// breedable_child 的帕鲁索引，按 (breeding_power, internal_index) 排序
    eligible: Vec<usize>,
}

impl BreedingDB {
    pub fn new(pals: Vec<Pal>, unique_combos: Vec<UniqueCombo>) -> Self {
        let by_name = pals
            .iter()
            .enumerate()
            .map(|(i, p)| (p.internal_name.clone(), i))
            .collect();
        let mut combo_index: HashMap<(String, String), Vec<usize>> = HashMap::new();
        for (i, c) in unique_combos.iter().enumerate() {
            combo_index
                .entry((c.parent1.clone(), c.parent2.clone()))
                .or_default()
                .push(i);
        }
        let mut eligible: Vec<usize> = pals
            .iter()
            .enumerate()
            .filter(|(_, p)| p.breedable_child)
            .map(|(i, _)| i)
            .collect();
        eligible.sort_by_key(|&i| (pals[i].breeding_power, pals[i].internal_index));
        Self {
            pals,
            unique_combos,
            by_name,
            combo_index,
            eligible,
        }
    }

    pub fn index_of(&self, internal_name: &str) -> Option<usize> {
        self.by_name.get(internal_name).copied()
    }

    pub fn pal(&self, internal_name: &str) -> Option<&Pal> {
        self.index_of(internal_name).map(|i| &self.pals[i])
    }

    /// 大小写不敏感的查找，命中时返回图鉴的规范 internal_name。
    /// 游戏内 FName 与图鉴命名可能差在大小写（GhostAnglerFish vs GhostAnglerfish）。
    pub fn canonical_name_ci(&self, name: &str) -> Option<&str> {
        self.pals
            .iter()
            .find(|p| p.internal_name.eq_ignore_ascii_case(name))
            .map(|p| p.internal_name.as_str())
    }

    /// 计算一对亲本的子代。p1、p2 为 internal_name。
    pub fn breed(&self, p1: &str, p2: &str) -> Option<BreedOutcome> {
        let a = self.pal(p1)?;
        let b = self.pal(p2)?;
        self.breed_inner(a, b)
    }

    /// 该无序配对是否为唯一组合（非性别规则）。供 planner 标记边类型。
    pub fn is_unique_pair(&self, p1: &str, p2: &str) -> bool {
        let (lo, hi) = ordered_pair(p1, p2);
        self.combo_index
            .get(&(lo, hi))
            .is_some_and(|idxs| idxs.iter().any(|&i| self.unique_combos[i].female_parent.is_none()))
    }

    /// 性别规则组合：Some((p1 为雌性时的子代, p2 为雌性时的子代))（pals 索引）。
    pub fn gender_combo_children(&self, p1: &str, p2: &str) -> Option<(usize, usize)> {
        let a = self.pal(p1)?;
        let b = self.pal(p2)?;
        match self.breed_inner(a, b) {
            Some(BreedOutcome::GenderDependent {
                if_p1_female,
                if_p2_female,
            }) => Some((if_p1_female, if_p2_female)),
            _ => None,
        }
    }

    /// 唯一组合产出的直接亲本对（不可达提示用）。返回 (parent1, parent2) 内部名。
    pub fn unique_parents_of(&self, child: &str) -> Vec<(String, String)> {
        self.unique_combos
            .iter()
            .filter(|c| c.child == child)
            .map(|c| (c.parent1.clone(), c.parent2.clone()))
            .collect()
    }

    fn breed_inner(&self, a: &Pal, b: &Pal) -> Option<BreedOutcome> {
        // 1. 唯一组合
        let (lo, hi) = ordered_pair(&a.internal_name, &b.internal_name);
        let combos: Vec<&UniqueCombo> = self
            .combo_index
            .get(&(lo, hi))
            .map(|idxs| idxs.iter().map(|&i| &self.unique_combos[i]).collect())
            .unwrap_or_default();
        match combos.len() {
            0 => {}
            1 if combos[0].female_parent.is_none() => {
                return Some(BreedOutcome::Normal(self.by_name[&combos[0].child]));
            }
            _ => {
                // 性别相关组合：两条记录，分别标注雌性亲本
                let mut if_a_female = None;
                let mut if_b_female = None;
                for c in &combos {
                    let child = self.by_name[&c.child];
                    match c.female_parent.as_deref() {
                        Some(fp) if fp == a.internal_name => if_a_female = Some(child),
                        Some(fp) if fp == b.internal_name => if_b_female = Some(child),
                        _ => {}
                    }
                }
                if let (Some(x), Some(y)) = (if_a_female, if_b_female) {
                    return Some(BreedOutcome::GenderDependent {
                        if_p1_female: x,
                        if_p2_female: y,
                    });
                }
                // 数据异常时退化为第一条记录
                return Some(BreedOutcome::Normal(self.by_name[&combos[0].child]));
            }
        }

        // 2. 同种必出同种
        if a.internal_name == b.internal_name {
            return Some(BreedOutcome::Normal(self.by_name[&a.internal_name]));
        }

        // 3. 公式
        if self.eligible.is_empty() {
            // 无公式可产出物种（仅见于合成测试数据；真实数据不会为空）
            return None;
        }
        Some(BreedOutcome::Normal(formula_child(&self.pals, &self.eligible, a, b)))
    }

    /// 反向查询：产出指定子代（internal_name）的全部无序亲本组合。
    /// 返回 (parent1, parent2, outcome) 列表，parent1/parent2 为 pals 索引。
    pub fn parents_of(&self, child: &str) -> Vec<(usize, usize, BreedOutcome)> {
        let Some(child_idx) = self.index_of(child) else {
            return Vec::new();
        };
        let n = self.pals.len();
        let mut out = Vec::new();
        for i in 0..n {
            for j in i..n {
                let Some(outcome) = self.breed_inner(&self.pals[i], &self.pals[j]) else {
                    continue;
                };
                let hit = match outcome {
                    BreedOutcome::Normal(c) => c == child_idx,
                    BreedOutcome::GenderDependent {
                        if_p1_female,
                        if_p2_female,
                    } => if_p1_female == child_idx || if_p2_female == child_idx,
                };
                if hit {
                    out.push((i, j, outcome));
                }
            }
        }
        out
    }
}

/// 公式核心：供 gen_data 复用（eligible 集合在推导期会变化）。
///
/// target = (rankA + rankB + 1) / 2 按实数比较（即 a+b 为偶数时，
/// floor/ceil 两个邻居等距，靠 internal_index 决胜）。
/// 用 2*power - (a+b+1) 的整数形式避免浮点。
pub fn formula_child(pals: &[Pal], eligible: &[usize], a: &Pal, b: &Pal) -> usize {
    let target2 = a.breeding_power + b.breeding_power + 1;
    *eligible
        .iter()
        .min_by_key(|&&i| {
            (
                (2 * pals[i].breeding_power - target2).abs(),
                pals[i].internal_index,
            )
        })
        .expect("eligible 集合不能为空")
}

fn ordered_pair(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

/// 从 JSON 文本构建数据库（app 侧用 include_str! 嵌入，gen_data 直接传字符串）。
pub fn from_json(pals_json: &str, combos_json: &str) -> serde_json::Result<BreedingDB> {
    let pals: Vec<Pal> = serde_json::from_str(pals_json)?;
    let combos: Vec<UniqueCombo> = serde_json::from_str(combos_json)?;
    Ok(BreedingDB::new(pals, combos))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pal(name: &str, power: i64, index: i64, breedable: bool) -> Pal {
        Pal {
            internal_name: name.into(),
            paldex_no: 1,
            is_variant: false,
            name_zh: name.into(),
            name_en: name.into(),
            breeding_power: power,
            internal_index: index,
            breedable_child: breedable,
        }
    }

    fn combo(p1: &str, p2: &str, child: &str, female_parent: Option<&str>) -> UniqueCombo {
        let (parent1, parent2) = if p1 <= p2 { (p1, p2) } else { (p2, p1) };
        UniqueCombo {
            parent1: parent1.into(),
            parent2: parent2.into(),
            child: child.into(),
            female_parent: female_parent.map(Into::into),
        }
    }

    /// A(100) + B(201) → target = 151（实数 (100+201+1)/2 = 151）
    /// 候选 X(140, idx 5) 距离 11，Y(162, idx 3) 距离 11 → 平局取 internal_index 小的 Y。
    #[test]
    fn formula_tie_breaks_by_internal_index() {
        let db = BreedingDB::new(
            vec![
                pal("A", 100, 1, false),
                pal("B", 201, 2, false),
                pal("X", 140, 5, true),
                pal("Y", 162, 3, true),
            ],
            vec![],
        );
        assert_eq!(db.breed("A", "B"), Some(BreedOutcome::Normal(3)));
    }

    /// 非 breedable_child 的帕鲁不能作为公式结果，但仍可作为亲本。
    #[test]
    fn formula_skips_non_breedable_children() {
        let db = BreedingDB::new(
            vec![
                pal("A", 100, 1, false),
                pal("B", 200, 2, false),
                pal("Legend", 150, 3, false), // 距离 0 但不可产出
                pal("Common", 160, 4, true),
            ],
            vec![],
        );
        assert_eq!(db.breed("A", "B"), Some(BreedOutcome::Normal(3)));
    }

    /// 唯一组合优先于公式与同种规则。
    #[test]
    fn unique_combo_beats_formula() {
        let db = BreedingDB::new(
            vec![
                pal("A", 100, 1, true),
                pal("B", 200, 2, true),
                pal("C", 150, 3, true),
                pal("Special", 9999, 4, false),
            ],
            vec![combo("A", "B", "Special", None)],
        );
        assert_eq!(db.breed("A", "B"), Some(BreedOutcome::Normal(3)));
        // 与参数顺序无关
        assert_eq!(db.breed("B", "A"), Some(BreedOutcome::Normal(3)));
    }

    #[test]
    fn same_species_breeds_same() {
        let db = BreedingDB::new(vec![pal("A", 100, 1, false)], vec![]);
        assert_eq!(db.breed("A", "A"), Some(BreedOutcome::Normal(0)));
    }

    #[test]
    fn gender_dependent_combo() {
        let db = BreedingDB::new(
            vec![
                pal("Wixen", 100, 1, false),
                pal("Katress", 200, 2, false),
                pal("WixenNoct", 300, 3, false),
                pal("KatressIgnis", 400, 4, false),
            ],
            vec![
                combo("Wixen", "Katress", "WixenNoct", Some("Wixen")),
                combo("Wixen", "Katress", "KatressIgnis", Some("Katress")),
            ],
        );
        assert_eq!(
            db.breed("Katress", "Wixen"),
            Some(BreedOutcome::GenderDependent {
                if_p1_female: 3, // Katress 为雌性 → KatressIgnis
                if_p2_female: 2, // Wixen 为雌性 → WixenNoct
            })
        );
    }

    #[test]
    fn parents_of_finds_all() {
        let db = BreedingDB::new(
            vec![
                pal("A", 100, 1, true),
                pal("B", 200, 2, true),
                pal("C", 150, 3, true),
            ],
            vec![],
        );
        let parents = db.parents_of("C");
        // A+B → C（公式）；A+C → target 125.5，C(150) 比 A(100) 近 → C；C+C → C
        assert_eq!(parents.len(), 3);
        assert!(parents.iter().any(|&(a, b, _)| a == 0 && b == 1));
        // A 仅由 A+A 产出
        assert_eq!(db.parents_of("A").len(), 1);
    }
}
