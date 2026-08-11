//! 配种路径规划：从已持有帕鲁出发，计算得到目标帕鲁的最优配种树。
//!
//! 算法：超图上的 Dijkstra。每个物种最多有两个"获得方式"条目：
//! - 已持有条目（cost 0，性别按登记合并，tie-break 分数来自已持有被动）；
//! - 配种条目（cost = 双亲 cost + 1，性别雌雄均可——后代性别假设，UI 会注明）。
//!
//! 按 (cost 升序, depth 升序, passive_score 降序) 逐个 finalize；
//! 每 finalize 一个条目，与所有已 finalize 条目尝试配对提议子代。
//! 由于子代 cost 严格大于双亲，该顺序对 cost 是精确最优；
//! depth 与 passive_score 仅作平局偏好。

use crate::breeding::{BreedOutcome, BreedingDB};
use serde::{Deserialize, Serialize};
use std::collections::BinaryHeap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Gender {
    Male,
    Female,
}

impl Gender {
    pub fn symbol(self) -> &'static str {
        match self {
            Gender::Male => "♂",
            Gender::Female => "♀",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OwnedPal {
    pub id: u64,
    /// 物种 internal_name
    pub species: String,
    pub gender: Gender,
    /// 被动技能 internal_name，最多 4 个
    pub passives: Vec<String>,
}

/// 一个规划目标：想要得到的帕鲁 + 期望继承的被动
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetGoal {
    pub id: u64,
    pub species: String,
    pub desired_passives: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BreedKind {
    Formula,
    Unique,
    GenderUnique,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanSource {
    Owned { pal_id: u64 },
    Bred {
        kind: BreedKind,
        p1: Box<PlanNode>,
        p2: Box<PlanNode>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanNode {
    pub species: String,
    /// 该节点作为亲本时需提供的性别；根节点为 None（任意）
    pub need_gender: Option<Gender>,
    pub source: PlanSource,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub root: PlanNode,
    pub total_breedings: u32,
    pub generations: u32,
    pub used_owned: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanError {
    UnknownSpecies(String),
    Unreachable {
        target: String,
        /// 目标的直接唯一组合亲本对（内部名），供 UI 给出捕捉建议
        unique_parents: Vec<(String, String)>,
    },
}

/// 一个物种的一种获得方式。
#[derive(Debug, Clone)]
struct Entry {
    cost: u32,
    depth: u32,
    /// 血统中已持有个体覆盖的期望被动位掩码（bit i = desired[i]）
    passive_mask: u64,
    male: bool,
    female: bool,
    via: Option<Via>,
}

/// 配种来源：两个亲本条目（species 索引, 是否为配种条目）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Via {
    a: (usize, bool),
    b: (usize, bool),
    kind: BreedKind,
}

/// 堆元素：按 key 最小弹出（最小堆语义由反向 cmp 实现）。
#[derive(Debug, PartialEq, Eq)]
struct Proposal {
    key: [u32; 3],
    child: usize,
    cost: u32,
    depth: u32,
    mask: u64,
    via: Via,
}

impl Ord for Proposal {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.key.cmp(&self.key)
    }
}

impl PartialOrd for Proposal {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

struct Planner<'a> {
    db: &'a BreedingDB,
    owned: &'a [OwnedPal],
    /// 期望被动（已去重，保序）
    desired: &'a [String],
    target: usize,
    /// false = 最少配种次数优先（覆盖作平局决胜）；true = 覆盖优先（次数作次优）
    coverage_first: bool,
    /// 每物种 [已持有条目, 配种条目]（仅已 finalize 的）
    entries: Vec<[Option<Entry>; 2]>,
    /// 已 finalize 的条目引用（species, is_bred）
    finalized: Vec<(usize, bool)>,
}

impl<'a> Planner<'a> {
    fn new(
        db: &'a BreedingDB,
        owned: &'a [OwnedPal],
        desired: &'a [String],
        target: usize,
        coverage_first: bool,
    ) -> Self {
        Self {
            db,
            owned,
            desired,
            target,
            coverage_first,
            entries: vec![[None, None]; db.pals.len()],
            finalized: Vec::new(),
        }
    }

    /// 一组被动覆盖期望的位掩码
    fn mask_of(&self, passives: &[String]) -> u64 {
        let mut mask = 0u64;
        for (i, d) in self.desired.iter().enumerate() {
            if passives.contains(d) {
                mask |= 1 << i;
            }
        }
        mask
    }

    /// 排序键：升序更优
    fn key(&self, cost: u32, depth: u32, mask: u64) -> [u32; 3] {
        let missing = self.desired.len() as u32 - mask.count_ones();
        if self.coverage_first {
            [missing, cost, depth]
        } else {
            [cost, depth, missing]
        }
    }

    /// 已持有帕鲁合并出的初始条目（cost 0）。
    fn owned_entry(&self, species_idx: usize) -> Option<Entry> {
        let species = &self.db.pals[species_idx].internal_name;
        let pals: Vec<&OwnedPal> = self.owned.iter().filter(|p| p.species == *species).collect();
        if pals.is_empty() {
            return None;
        }
        let male = pals.iter().any(|p| p.gender == Gender::Male);
        let female = pals.iter().any(|p| p.gender == Gender::Female);
        let mask = pals
            .iter()
            .fold(0u64, |m, p| m | self.mask_of(&p.passives));
        Some(Entry {
            cost: 0,
            depth: 0,
            passive_mask: mask,
            male,
            female,
            via: None,
        })
    }

    fn entry(&self, r: (usize, bool)) -> &Entry {
        self.entries[r.0][r.1 as usize].as_ref().expect("条目已 finalize")
    }

    /// 尝试用条目 ea（物种 ia）× eb（物种 ib）提议子代。
    fn propose_pair(
        &self,
        heap: &mut BinaryHeap<Proposal>,
        ra: (usize, bool),
        rb: (usize, bool),
    ) {
        let (ia, ib) = (ra.0, rb.0);
        if ia == ib {
            return; // 同种配种不产生新物种
        }
        let ea = self.entry(ra);
        let eb = self.entry(rb);
        let a = &self.db.pals[ia];
        let b = &self.db.pals[ib];
        // 性别可行性：凑齐一雌一雄
        if !(ea.male || eb.male) || !(ea.female || eb.female) {
            return;
        }
        let cost = ea.cost + eb.cost + 1;
        let depth = ea.depth.max(eb.depth) + 1;
        let mask = ea.passive_mask | eb.passive_mask;
        match self.db.breed(&a.internal_name, &b.internal_name) {
            Some(BreedOutcome::Normal(child)) => {
                let kind = if self.db.is_unique_pair(&a.internal_name, &b.internal_name) {
                    BreedKind::Unique
                } else {
                    BreedKind::Formula
                };
                self.push(heap, child, cost, depth, mask, Via { a: ra, b: rb, kind });
            }
            Some(BreedOutcome::GenderDependent {
                if_p1_female,
                if_p2_female,
            }) => {
                // 子代取决于哪只亲本是雌性；对应亲本条目必须有雌性
                if ea.female {
                    self.push(heap, if_p1_female, cost, depth, mask, Via {
                        a: ra,
                        b: rb,
                        kind: BreedKind::GenderUnique,
                    });
                }
                if eb.female && if_p2_female != if_p1_female {
                    self.push(heap, if_p2_female, cost, depth, mask, Via {
                        a: ra,
                        b: rb,
                        kind: BreedKind::GenderUnique,
                    });
                }
            }
            None => {}
        }
    }

    fn push(
        &self,
        heap: &mut BinaryHeap<Proposal>,
        child: usize,
        cost: u32,
        depth: u32,
        mask: u64,
        via: Via,
    ) {
        let key = self.key(cost, depth, mask);
        // 已有更优 finalize 条目则不必提议
        if let Some(e) = &self.entries[child][1] {
            if self.key(e.cost, e.depth, e.passive_mask) <= key {
                return;
            }
        }
        // 已持有（cost 0）的物种不需要配种获得——但目标物种允许：
        // 已持有却被动不符时，必须能通过配种重新获得目标
        if self.entries[child][0].is_some() && child != self.target {
            return;
        }
        heap.push(Proposal {
            key,
            child,
            cost,
            depth,
            mask,
            via,
        });
    }

    /// Dijkstra 主循环。返回目标的最优配种条目（已持有情形由调用方处理）。
    fn run(&mut self) -> Option<Entry> {
        let mut heap = BinaryHeap::new();
        // 初始：所有已持有条目
        for i in 0..self.db.pals.len() {
            if let Some(e) = self.owned_entry(i) {
                self.entries[i][0] = Some(e);
                self.finalized.push((i, false));
            }
        }
        // 已持有条目之间也要互相配对：逐个与之前的 finalize 列表配对
        let initial: Vec<(usize, bool)> = self.finalized.clone();
        for (k, &r) in initial.iter().enumerate() {
            for &other in &initial[..k] {
                self.propose_pair(&mut heap, r, other);
            }
        }
        while let Some(p) = heap.pop() {
            if self.entries[p.child][1].is_some() {
                continue; // 已 finalize 更优解
            }
            let r = (p.child, true);
            self.entries[p.child][1] = Some(Entry {
                cost: p.cost,
                depth: p.depth,
                passive_mask: p.mask,
                male: true,
                female: true, // 后代性别假设：雌雄均可获得
                via: Some(p.via),
            });
            self.finalized.push(r);
            if p.child == self.target {
                break; // 目标已最优，提前结束
            }
            let finalized = self.finalized.clone();
            for other in finalized {
                if other != r {
                    self.propose_pair(&mut heap, r, other);
                }
            }
        }
        self.entries[self.target][1].clone()
    }

    /// 自底向上重建树并分配性别。
    fn build_node(&self, r: (usize, bool), need: Option<Gender>) -> PlanNode {
        let species = self.db.pals[r.0].internal_name.clone();
        let e = self.entry(r);
        match e.via {
            None => {
                let pal = self.pick_owned(r.0, need);
                PlanNode {
                    species,
                    need_gender: need,
                    source: PlanSource::Owned { pal_id: pal.id },
                }
            }
            Some(via) => {
                let (ga, gb) = self.assign_genders(via, r.0);
                PlanNode {
                    species,
                    need_gender: need,
                    source: PlanSource::Bred {
                        kind: via.kind,
                        p1: Box::new(self.build_node(via.a, Some(ga))),
                        p2: Box::new(self.build_node(via.b, Some(gb))),
                    },
                }
            }
        }
    }

    /// 为配种双亲分配性别。
    fn assign_genders(&self, via: Via, child: usize) -> (Gender, Gender) {
        let ea = self.entry(via.a);
        let eb = self.entry(via.b);
        if via.kind == BreedKind::GenderUnique {
            // 哪只亲本为雌性由子代决定
            let a = &self.db.pals[via.a.0].internal_name;
            let b = &self.db.pals[via.b.0].internal_name;
            if let Some((if_a_female, _)) = self.db.gender_combo_children(a, b) {
                if if_a_female == child {
                    return (Gender::Female, Gender::Male);
                }
            }
            return (Gender::Male, Gender::Female);
        }
        match (ea.male, ea.female, eb.male, eb.female) {
            (true, false, _, _) => (Gender::Male, Gender::Female),
            (_, _, true, false) => (Gender::Female, Gender::Male),
            (false, true, _, _) => (Gender::Female, Gender::Male),
            (_, _, false, true) => (Gender::Male, Gender::Female),
            // 两边性别都齐全：优先让已持有叶子保持登记性别
            _ => (Gender::Male, Gender::Female),
        }
    }

    /// 选出符合物种与性别要求的已持有个体。
    fn pick_owned(&self, species_idx: usize, need: Option<Gender>) -> OwnedPal {
        let species = &self.db.pals[species_idx].internal_name;
        let mut candidates: Vec<&OwnedPal> = self
            .owned
            .iter()
            .filter(|p| p.species == *species)
            .collect();
        if let Some(g) = need {
            let matched: Vec<&OwnedPal> = candidates
                .iter()
                .copied()
                .filter(|p| p.gender == g)
                .collect();
            if !matched.is_empty() {
                candidates = matched;
            }
        }
        // 优先携带期望被动的个体
        candidates
            .into_iter()
            .max_by_key(|p| {
                p.passives
                    .iter()
                    .filter(|ps| self.desired.contains(ps))
                    .count()
            })
            .expect("已持有条目必然存在对应个体")
            .clone()
    }
}

fn collect_stats(root: &PlanNode, breedings: &mut u32, used: &mut Vec<u64>) -> u32 {
    match &root.source {
        PlanSource::Owned { pal_id } => {
            used.push(*pal_id);
            0
        }
        PlanSource::Bred { p1, p2, .. } => {
            *breedings += 1;
            let d1 = collect_stats(p1, breedings, used);
            let d2 = collect_stats(p2, breedings, used);
            d1.max(d2) + 1
        }
    }
}

impl Planner<'_> {
    /// 由最终条目重建 Plan（统计配种次数、世代、用到的已持有帕鲁）。
    fn into_plan(self, target: usize, entry: &Entry) -> Plan {
        let root_ref = (target, entry.via.is_some());
        let root = self.build_node(root_ref, None);
        let mut breedings = 0;
        let mut used = Vec::new();
        let generations = collect_stats(&root, &mut breedings, &mut used);
        used.sort_unstable();
        used.dedup();
        Plan {
            root,
            total_breedings: breedings,
            generations,
            used_owned: used,
        }
    }

    /// 已持有目标帕鲁时的平凡结果（0 次配种）。
    fn trivial_plan(self, target: usize) -> Plan {
        let pal = self.pick_owned(target, None);
        Plan {
            root: PlanNode {
                species: self.db.pals[target].internal_name.clone(),
                need_gender: None,
                source: PlanSource::Owned { pal_id: pal.id },
            },
            total_breedings: 0,
            generations: 0,
            used_owned: vec![pal.id],
        }
    }
}

/// 计算从已持有帕鲁到目标物种的最优配种路径。
///
/// 被动语义：已持有目标但单只未覆盖全部期望被动时视为"未达成"，
/// 转而求配种路径。两级求解：先求"最少配种次数"的路径，若其血统能覆盖
/// 全部期望被动则直接采用；否则再求"覆盖优先"的路径（可接受更多次数），
/// 两者取覆盖更好者（平局取次数少者）。覆盖优先为启发式，非严格最优。
pub fn plan(
    db: &BreedingDB,
    owned: &[OwnedPal],
    target: &str,
    desired_passives: &[String],
) -> Result<Plan, PlanError> {
    let Some(target_idx) = db.index_of(target) else {
        return Err(PlanError::UnknownSpecies(target.to_string()));
    };
    // 期望被动去重（保序），掩码位与下标对应
    let mut desired: Vec<String> = Vec::new();
    for d in desired_passives {
        if !desired.contains(d) {
            desired.push(d.clone());
        }
    }

    // 已持有目标且单只覆盖全部期望 → 直接完成（0 次配种）
    let satisfied = owned.iter().any(|p| {
        p.species == target && desired.iter().all(|d| p.passives.contains(d))
    });
    if satisfied {
        return Ok(Planner::new(db, owned, &desired, target_idx, false).trivial_plan(target_idx));
    }

    // A：最少配种次数优先
    let mut a = Planner::new(db, owned, &desired, target_idx, false);
    let best_a = a.run();
    let full_coverage = |e: &Entry| e.passive_mask.count_ones() as usize == desired.len();
    if let Some(e) = &best_a {
        if full_coverage(e) {
            let e = e.clone();
            return Ok(a.into_plan(target_idx, &e));
        }
    }

    // B：覆盖优先（A 未全覆盖时）
    if !desired.is_empty() {
        let mut b = Planner::new(db, owned, &desired, target_idx, true);
        if let Some(eb) = b.run() {
            let better = match &best_a {
                None => true,
                Some(ea) => {
                    (eb.passive_mask.count_ones(), std::cmp::Reverse(eb.cost))
                        > (ea.passive_mask.count_ones(), std::cmp::Reverse(ea.cost))
                }
            };
            if better {
                return Ok(b.into_plan(target_idx, &eb));
            }
        }
    }

    if let Some(e) = best_a {
        return Ok(a.into_plan(target_idx, &e));
    }
    // 没有可行配种路径：已持有（被动不符）则如实展示，否则报告不可达
    if owned.iter().any(|p| p.species == target) {
        return Ok(Planner::new(db, owned, &desired, target_idx, false).trivial_plan(target_idx));
    }
    Err(PlanError::Unreachable {
        target: target.to_string(),
        unique_parents: db.unique_parents_of(target),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::breeding::{Pal, UniqueCombo};

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

    fn owned(id: u64, species: &str, gender: Gender, passives: &[&str]) -> OwnedPal {
        OwnedPal {
            id,
            species: species.into(),
            gender,
            passives: passives.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// A(100)×B(200) → C(150)；C(150)×D(240) → E(200)
    /// 数据设计：A×D 平均值 170.5 → C（不是 E）；B×D 平均 220.5 → E，
    /// 但测试里 B♀ × D♀ 性别不可行，故唯一最优路径是两步链。
    fn sample_db() -> BreedingDB {
        BreedingDB::new(
            vec![
                pal("A", 100, 1, false),
                pal("B", 200, 2, false),
                pal("C", 150, 3, true),
                pal("D", 240, 4, false),
                pal("E", 200, 5, true),
            ],
            vec![],
        )
    }

    /// 在配种节点中按物种名找子节点
    fn child_by_species<'t>(node: &'t PlanNode, species: &str) -> &'t PlanNode {
        let PlanSource::Bred { p1, p2, .. } = &node.source else {
            panic!("应为配种节点");
        };
        if p1.species == species {
            p1
        } else if p2.species == species {
            p2
        } else {
            panic!("找不到子节点 {species}");
        }
    }

    #[test]
    fn plans_a_two_step_chain() {
        let db = sample_db();
        // B♀ × D♀ 性别不可行，必须走 A×B→C、C×D→E 的链
        let owned = vec![
            owned(1, "A", Gender::Male, &[]),
            owned(2, "B", Gender::Female, &[]),
            owned(3, "D", Gender::Female, &[]),
        ];
        let plan = plan(&db, &owned, "E", &[]).unwrap();
        assert_eq!(plan.total_breedings, 2);
        assert_eq!(plan.generations, 2);
        // 最优解有两种（C×D 或 B×C），都经过 C=A×B；不断言具体选择
        let c_node = child_by_species(&plan.root, "C");
        assert!(matches!(
            child_by_species(c_node, "A").source,
            PlanSource::Owned { pal_id: 1 }
        ));
        assert!(matches!(
            child_by_species(c_node, "B").source,
            PlanSource::Owned { pal_id: 2 }
        ));
        assert!(plan.used_owned.contains(&1) && plan.used_owned.contains(&2));
    }

    #[test]
    fn target_already_owned_is_trivial() {
        let db = sample_db();
        let owned = vec![owned(7, "A", Gender::Male, &[])];
        let plan = plan(&db, &owned, "A", &[]).unwrap();
        assert_eq!(plan.total_breedings, 0);
        assert_eq!(plan.generations, 0);
        assert_eq!(plan.used_owned, vec![7]);
    }

    #[test]
    fn same_gender_pair_cannot_breed() {
        let db = sample_db();
        let owned = vec![
            owned(1, "A", Gender::Male, &[]),
            owned(2, "B", Gender::Male, &[]),
        ];
        let err = plan(&db, &owned, "C", &[]).unwrap_err();
        assert!(matches!(err, PlanError::Unreachable { .. }));
    }

    #[test]
    fn gender_assignment_respects_owned_genders() {
        let db = sample_db();
        let owned = vec![
            owned(1, "A", Gender::Male, &[]),
            owned(2, "B", Gender::Female, &[]),
        ];
        let plan = plan(&db, &owned, "C", &[]).unwrap();
        // 双亲顺序由堆弹出顺序决定，按物种断言
        assert_eq!(
            child_by_species(&plan.root, "A").need_gender,
            Some(Gender::Male)
        );
        assert_eq!(
            child_by_species(&plan.root, "B").need_gender,
            Some(Gender::Female)
        );
    }

    #[test]
    fn unique_combo_marks_kind() {
        let db = BreedingDB::new(
            vec![
                pal("X", 100, 1, false),
                pal("Y", 200, 2, false),
                pal("Z", 999, 3, false),
            ],
            vec![UniqueCombo {
                parent1: "X".into(),
                parent2: "Y".into(),
                child: "Z".into(),
                female_parent: None,
            }],
        );
        let owned = vec![
            owned(1, "X", Gender::Male, &[]),
            owned(2, "Y", Gender::Female, &[]),
        ];
        let plan = plan(&db, &owned, "Z", &[]).unwrap();
        let PlanSource::Bred { kind, .. } = &plan.root.source else {
            panic!();
        };
        assert_eq!(*kind, BreedKind::Unique);
    }

    #[test]
    fn passive_preference_breaks_ties() {
        // A×B → C，A'×B → C（两种获得 C 的方式，cost 相同）
        let db = BreedingDB::new(
            vec![
                pal("A", 100, 1, false),
                pal("A2", 110, 2, false),
                pal("B", 200, 3, false),
                pal("C", 155, 4, true),
            ],
            vec![],
        );
        let owned = vec![
            owned(1, "A", Gender::Male, &[]),
            owned(2, "A2", Gender::Male, &["lucky"]),
            owned(3, "B", Gender::Female, &[]),
        ];
        let plan = plan(&db, &owned, "C", &["lucky".to_string()]).unwrap();
        // 应选择携带 lucky 的 A2
        assert!(plan.used_owned.contains(&2));
        assert!(!plan.used_owned.contains(&1));
    }

    #[test]
    fn unreachable_reports_unique_parents() {
        let db = BreedingDB::new(
            vec![
                pal("X", 100, 1, false),
                pal("Y", 200, 2, false),
                pal("L", 50, 3, false), // 不可被公式产出
            ],
            vec![UniqueCombo {
                parent1: "X".into(),
                parent2: "Y".into(),
                child: "L".into(),
                female_parent: None,
            }],
        );
        let err = plan(&db, &[], "L", &[]).unwrap_err();
        match err {
            PlanError::Unreachable { unique_parents, .. } => {
                assert_eq!(unique_parents, vec![("X".to_string(), "Y".to_string())]);
            }
            _ => panic!(),
        }
        // 拥有亲本后则可达
        let owned = vec![
            owned(1, "X", Gender::Male, &[]),
            owned(2, "Y", Gender::Female, &[]),
        ];
        assert!(plan(&db, &owned, "L", &[]).is_ok());
    }

    #[test]
    fn gender_unique_combo_requires_female_parent() {
        let db = BreedingDB::new(
            vec![
                pal("P", 100, 1, false),
                pal("Q", 200, 2, false),
                pal("R1", 300, 3, false),
                pal("R2", 400, 4, false),
            ],
            vec![
                UniqueCombo {
                    parent1: "P".into(),
                    parent2: "Q".into(),
                    child: "R1".into(),
                    female_parent: Some("P".into()),
                },
                UniqueCombo {
                    parent1: "P".into(),
                    parent2: "Q".into(),
                    child: "R2".into(),
                    female_parent: Some("Q".into()),
                },
            ],
        );
        // P 为雌性 → R1，且 P 需为雌性
        let pair = vec![
            owned(1, "P", Gender::Female, &[]),
            owned(2, "Q", Gender::Male, &[]),
        ];
        let result = plan(&db, &pair, "R1", &[]).unwrap();
        let PlanSource::Bred { kind, .. } = &result.root.source else {
            panic!();
        };
        assert_eq!(*kind, BreedKind::GenderUnique);
        assert_eq!(
            child_by_species(&result.root, "P").need_gender,
            Some(Gender::Female)
        );
        assert_eq!(
            child_by_species(&result.root, "Q").need_gender,
            Some(Gender::Male)
        );
        // P 无雌性则 R1 不可达（Q 雌性产出的是 R2）
        let pair_m = vec![
            owned(1, "P", Gender::Male, &[]),
            owned(2, "Q", Gender::Male, &[]),
        ];
        assert!(plan(&db, &pair_m, "R1", &[]).is_err());
    }

    /// 已持有目标但被动不满足期望 → 视为未达成，返回配种路径。
    #[test]
    fn owned_target_with_wrong_passives_plans_breeding() {
        let db = sample_db(); // A(100)×B(200) → C(150)
        let owned = vec![
            owned(1, "A", Gender::Male, &[]),
            owned(2, "B", Gender::Female, &[]),
            owned(3, "C", Gender::Female, &[]), // 已持有 C 但不带 lucky
        ];
        let plan = plan(&db, &owned, "C", &["lucky".to_string()]).unwrap();
        // 不是平凡结果：应给出 A×B→C 的配种路径
        assert!(matches!(plan.root.source, PlanSource::Bred { .. }));
        assert_eq!(plan.total_breedings, 1);
    }

    /// 已持有目标且单只覆盖全部期望 → 平凡结果（0 次配种）。
    #[test]
    fn owned_target_with_all_desired_is_trivial() {
        let db = sample_db();
        let owned = vec![owned(3, "C", Gender::Female, &["lucky", "brave"])];
        let plan = plan(
            &db,
            &owned,
            "C",
            &["lucky".to_string(), "brave".to_string()],
        )
        .unwrap();
        assert_eq!(plan.total_breedings, 0);
        assert_eq!(plan.used_owned, vec![3]);
    }

    /// 覆盖优先：便宜但不带被动的路径应让位于多一步但全覆盖的路径。
    #[test]
    fn coverage_first_upgrades_to_longer_path() {
        // A(100)×B(300) → T(200)（cost 1，无被动）
        // A×C(199) → M(150)；M×B → T（cost 2，C 携带 lucky → 覆盖）
        let db = BreedingDB::new(
            vec![
                pal("A", 100, 1, false),
                pal("B", 300, 2, false),
                pal("C", 199, 3, false),
                pal("M", 150, 4, true),
                pal("T", 200, 5, true),
            ],
            vec![],
        );
        let owned = vec![
            owned(1, "A", Gender::Male, &[]),
            owned(2, "B", Gender::Female, &[]),
            owned(3, "C", Gender::Female, &["lucky"]),
        ];
        let plan = plan(&db, &owned, "T", &["lucky".to_string()]).unwrap();
        assert_eq!(plan.total_breedings, 2, "应选择多一次配种但覆盖 lucky 的路径");
        assert!(plan.used_owned.contains(&3));
    }
}
