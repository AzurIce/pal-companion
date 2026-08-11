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
use std::cmp::Reverse;
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
    /// tie-break：血统中已持有个体携带期望被动的累计数
    passive_score: u32,
    male: bool,
    female: bool,
    via: Option<Via>,
}

/// 配种来源：两个亲本条目（species 索引, 是否为配种条目）。
/// Ord 派生仅供堆元素占位，不参与语义比较。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Via {
    a: (usize, bool),
    b: (usize, bool),
    kind: BreedKind,
}

/// 堆元素：Reverse 让最小 (cost, depth, Reverse(score)) 先弹出。
type Proposal = Reverse<(u32, u32, Reverse<u32>, usize, Via)>;

struct Planner<'a> {
    db: &'a BreedingDB,
    owned: &'a [OwnedPal],
    desired: &'a [String],
    /// 每物种 [已持有条目, 配种条目]（仅已 finalize 的）
    entries: Vec<[Option<Entry>; 2]>,
    /// 已 finalize 的条目引用（species, is_bred）
    finalized: Vec<(usize, bool)>,
}

impl<'a> Planner<'a> {
    fn new(db: &'a BreedingDB, owned: &'a [OwnedPal], desired: &'a [String]) -> Self {
        Self {
            db,
            owned,
            desired,
            entries: vec![[None, None]; db.pals.len()],
            finalized: Vec::new(),
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
        let score = pals
            .iter()
            .flat_map(|p| p.passives.iter())
            .filter(|ps| self.desired.contains(ps))
            .count() as u32;
        Some(Entry {
            cost: 0,
            depth: 0,
            passive_score: score,
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
        let score = ea.passive_score + eb.passive_score;
        match self.db.breed(&a.internal_name, &b.internal_name) {
            Some(BreedOutcome::Normal(child)) => {
                let kind = if self.db.is_unique_pair(&a.internal_name, &b.internal_name) {
                    BreedKind::Unique
                } else {
                    BreedKind::Formula
                };
                self.push(heap, child, cost, depth, score, Via { a: ra, b: rb, kind });
            }
            Some(BreedOutcome::GenderDependent {
                if_p1_female,
                if_p2_female,
            }) => {
                // 子代取决于哪只亲本是雌性；对应亲本条目必须有雌性
                if ea.female {
                    self.push(heap, if_p1_female, cost, depth, score, Via {
                        a: ra,
                        b: rb,
                        kind: BreedKind::GenderUnique,
                    });
                }
                if eb.female && if_p2_female != if_p1_female {
                    self.push(heap, if_p2_female, cost, depth, score, Via {
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
        score: u32,
        via: Via,
    ) {
        // 已有更优 finalize 条目则不必提议
        if let Some(e) = &self.entries[child][1] {
            if (e.cost, e.depth, Reverse(e.passive_score)) <= (cost, depth, Reverse(score)) {
                return;
            }
        }
        // 已持有（cost 0）的物种不需要配种获得
        if self.entries[child][0].is_some() {
            return;
        }
        heap.push(Reverse((cost, depth, Reverse(score), child, via)));
    }

    fn run(&mut self, target: usize) -> Option<Entry> {
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
        while let Some(Reverse((cost, depth, Reverse(score), child, via))) = heap.pop() {
            if self.entries[child][1].is_some() {
                continue; // 已 finalize 更优解
            }
            let r = (child, true);
            self.entries[child][1] = Some(Entry {
                cost,
                depth,
                passive_score: score,
                male: true,
                female: true, // 后代性别假设：雌雄均可获得
                via: Some(via),
            });
            self.finalized.push(r);
            if child == target {
                break; // 目标已最优，提前结束
            }
            let finalized = self.finalized.clone();
            for other in finalized {
                if other != r {
                    self.propose_pair(&mut heap, r, other);
                }
            }
        }
        // 目标可能由已持有直接满足
        if let Some(e) = &self.entries[target][0] {
            return Some(e.clone());
        }
        self.entries[target][1].clone()
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

/// 计算从已持有帕鲁到目标物种的最优配种路径。
pub fn plan(
    db: &BreedingDB,
    owned: &[OwnedPal],
    target: &str,
    desired_passives: &[String],
) -> Result<Plan, PlanError> {
    let Some(target_idx) = db.index_of(target) else {
        return Err(PlanError::UnknownSpecies(target.to_string()));
    };
    let mut planner = Planner::new(db, owned, desired_passives);
    let Some(entry) = planner.run(target_idx) else {
        return Err(PlanError::Unreachable {
            target: target.to_string(),
            unique_parents: db.unique_parents_of(target),
        });
    };
    let root_ref = (target_idx, entry.via.is_some());
    let root = planner.build_node(root_ref, None);
    let mut breedings = 0;
    let mut used = Vec::new();
    let generations = collect_stats(&root, &mut breedings, &mut used);
    used.sort_unstable();
    used.dedup();
    Ok(Plan {
        root,
        total_breedings: breedings,
        generations,
        used_owned: used,
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
}
