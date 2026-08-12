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
use std::collections::{BinaryHeap, HashMap, HashSet};

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
    /// 所在位置（队伍/盒子/据点）；旧存档与手动登记默认盒子
    #[serde(default)]
    pub group: PalGroup,
    /// 是否头领（阿尔法）帕鲁；同步时按 BOSS_ 物种前缀标记
    #[serde(default)]
    pub is_boss: bool,
    /// 最爱标记：0=无，1/2/3 对应游戏内 I/II/III
    #[serde(default)]
    pub favorite: u8,
    /// 幸运（闪光）帕鲁标记，对应游戏 IsRarePal
    #[serde(default)]
    pub is_lucky: bool,
    /// 游戏内昵称；未命名 → None（展示时回退物种名）
    #[serde(default)]
    pub nickname: Option<String>,
}

/// 帕鲁所在位置（同步自游戏的容器分组）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PalGroup {
    Party,
    #[default]
    Box,
    Base,
}

impl PalGroup {
    pub fn label(self) -> &'static str {
        match self {
            PalGroup::Party => "队伍",
            PalGroup::Box => "盒子",
            PalGroup::Base => "据点",
        }
    }
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
    /// 树中各配种物种的可选亲本组合（按 (cost, depth, 覆盖) 排序），
    /// 供 UI 在节点上切换。键为物种 internal_name。
    pub alternatives: HashMap<String, Vec<Alternative>>,
}

/// 某物种的一个可选亲本组合
#[derive(Debug, Clone, PartialEq)]
pub struct Alternative {
    /// 无序亲本对（字典序）
    pub parents: (String, String),
    pub kind: BreedKind,
    pub cost: u32,
    pub depth: u32,
    /// 该组合血统覆盖的期望被动数
    pub covered: u32,
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
    /// 血统被动池（bit = pidx[被动]）：继承池去重后的全部词条。
    /// 池越小，期望词条的继承成功率越高（机制：去重并集、均匀无放回）
    pool: u128,
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
    pool: u128,
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
    /// 全局被动 → 池位（覆盖所有已持有帕鲁出现过的词条）
    pidx: &'a HashMap<String, u32>,
    /// 用户钉选的亲本对：物种 internal_name → 无序亲本对（字典序）
    pins: &'a HashMap<String, (String, String)>,
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
        pidx: &'a HashMap<String, u32>,
        pins: &'a HashMap<String, (String, String)>,
        target: usize,
        coverage_first: bool,
    ) -> Self {
        Self {
            db,
            owned,
            desired,
            pidx,
            pins,
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

    /// 全部词条（含非期望）的池位掩码
    fn pool_of(&self, passives: &[String]) -> u128 {
        passives.iter().fold(0u128, |m, p| {
            match self.pidx.get(p) {
                Some(&i) => m | (1u128 << i),
                None => m,
            }
        })
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
        // 掩码取单只最大值而非并集：一次配种只能用一只个体，
        // 并集会虚报覆盖（同种两只互补个体的合并由同种配种候选正确处理）。
        // 池同样按该个体的词条计（覆盖最高者，平局取池更小的）。
        let best = pals
            .iter()
            .max_by_key(|p| {
                (
                    self.mask_of(&p.passives).count_ones(),
                    std::cmp::Reverse(self.pool_of(&p.passives).count_ones()),
                )
            })
            .expect("非空");
        let mask = self.mask_of(&best.passives);
        let pool = self.pool_of(&best.passives);
        Some(Entry {
            cost: 0,
            depth: 0,
            passive_mask: mask,
            pool,
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
        let pool = ea.pool | eb.pool;
        match self.db.breed(&a.internal_name, &b.internal_name) {
            Some(BreedOutcome::Normal(child)) => {
                let kind = if self.db.is_unique_pair(&a.internal_name, &b.internal_name) {
                    BreedKind::Unique
                } else {
                    BreedKind::Formula
                };
                self.push(heap, child, cost, depth, mask, pool, Via { a: ra, b: rb, kind });
            }
            Some(BreedOutcome::GenderDependent {
                if_p1_female,
                if_p2_female,
            }) => {
                // 子代取决于哪只亲本是雌性；对应亲本条目必须有雌性
                if ea.female {
                    self.push(heap, if_p1_female, cost, depth, mask, pool, Via {
                        a: ra,
                        b: rb,
                        kind: BreedKind::GenderUnique,
                    });
                }
                if eb.female && if_p2_female != if_p1_female {
                    self.push(heap, if_p2_female, cost, depth, mask, pool, Via {
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
        pool: u128,
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
            pool,
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
                pool: p.pool,
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
    /// 自底向上重建树并分配性别。钉选在重建阶段局部生效：
    /// 只替换被钉选节点的亲本对，其余节点保持搜索得出的最优 via。
    fn build_node(
        &self,
        r: (usize, bool),
        need: Option<Gender>,
        ancestors: &mut Vec<usize>,
    ) -> PlanNode {
        let species = self.db.pals[r.0].internal_name.clone();
        let e = self.entry(r);
        let mut via = e.via;
        if let Some((pa, pb)) = self.pins.get(&species) {
            let (pa, pb) = (pa.clone(), pb.clone());
            if let Some(v) = self.resolve_pin(r.0, &pa, &pb, ancestors) {
                via = Some(v);
            }
        }
        match via {
            None => {
                let pal = self.pick_owned(r.0, need);
                PlanNode {
                    species,
                    need_gender: need,
                    source: PlanSource::Owned { pal_id: pal.id },
                }
            }
            Some(v) => {
                let (ga, gb) = self.assign_genders(v, r.0);
                ancestors.push(r.0);
                let p1 = self.build_node(v.a, Some(ga), ancestors);
                let p2 = self.build_node(v.b, Some(gb), ancestors);
                ancestors.pop();
                PlanNode {
                    species,
                    need_gender: need,
                    source: PlanSource::Bred {
                        kind: v.kind,
                        p1: Box::new(p1),
                        p2: Box::new(p2),
                    },
                }
            }
        }
    }

    /// 把钉选的亲本对解析为 Via：要求产出正确、双亲可用、性别可行、不成环。
    /// 不满足则返回 None（视为未钉选，回退到最优 via）。
    fn resolve_pin(
        &self,
        species: usize,
        pa: &str,
        pb: &str,
        ancestors: &[usize],
    ) -> Option<Via> {
        if ancestors.contains(&species) {
            return None;
        }
        // 同种配种：仅当前物种、已持有雌雄各一；双亲都是已持有叶子
        if pa == pb {
            if pa != self.db.pals[species].internal_name {
                return None;
            }
            let e = self.entries[species][0].as_ref()?;
            if e.male && e.female {
                return Some(Via {
                    a: (species, false),
                    b: (species, false),
                    kind: BreedKind::Formula,
                });
            }
            return None;
        }
        let ia = self.db.index_of(pa)?;
        let ib = self.db.index_of(pb)?;
        if ia == ib || ia == species || ib == species {
            return None;
        }
        if ancestors.contains(&ia) || ancestors.contains(&ib) {
            return None;
        }
        let (ea, a_bred) = entry_with_kind(&self.entries[ia])?;
        let (eb, b_bred) = entry_with_kind(&self.entries[ib])?;
        if !(ea.male || eb.male) || !(ea.female || eb.female) {
            return None;
        }
        match self.db.breed(pa, pb) {
            Some(BreedOutcome::Normal(c)) if c == species => {
                let kind = if self.db.is_unique_pair(pa, pb) {
                    BreedKind::Unique
                } else {
                    BreedKind::Formula
                };
                Some(Via {
                    a: (ia, a_bred),
                    b: (ib, b_bred),
                    kind,
                })
            }
            Some(BreedOutcome::GenderDependent {
                if_p1_female,
                if_p2_female,
            }) => {
                if if_p1_female == species && ea.female {
                    Some(Via {
                        a: (ia, a_bred),
                        b: (ib, b_bred),
                        kind: BreedKind::GenderUnique,
                    })
                } else if if_p2_female == species && eb.female {
                    Some(Via {
                        a: (ia, a_bred),
                        b: (ib, b_bred),
                        kind: BreedKind::GenderUnique,
                    })
                } else {
                    None
                }
            }
            _ => None,
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

/// 取物种的可用条目（优先配种条目，其次已持有），并标注来源。
fn entry_with_kind(slots: &[Option<Entry>; 2]) -> Option<(&Entry, bool)> {
    match (&slots[1], &slots[0]) {
        (Some(bred), _) => Some((bred, true)),
        (None, Some(owned)) => Some((owned, false)),
        _ => None,
    }
}

impl Planner<'_> {
    /// 枚举路径树中各配种物种的可选亲本组合（基于已 finalize 的条目）。
    /// 必须在 into_plan 之前调用（self 尚未消耗且 run 已完成）。
    fn collect_alternatives(&self, target: usize) -> HashMap<String, Vec<Alternative>> {
        // 收集树中出现的配种物种
        let mut tree_species: HashSet<usize> = HashSet::new();
        let mut stack = vec![target];
        while let Some(s) = stack.pop() {
            if let Some(Some(e)) = self.entries.get(s).map(|e| &e[1]) {
                if tree_species.insert(s) {
                    if let Some(via) = e.via {
                        stack.push(via.a.0);
                        stack.push(via.b.0);
                    }
                }
            }
        }
        let mut out = HashMap::new();
        for &s in &tree_species {
            let name = self.db.pals[s].internal_name.clone();
            let mut alts = Vec::new();
            // 同种配种（合并被动）：已持有该物种且雌雄兼备时也是合法方案
            if let Some(e) = &self.entries[s][0] {
                if e.male && e.female {
                    let males: Vec<&OwnedPal> = self
                        .owned
                        .iter()
                        .filter(|p| p.species == name && p.gender == Gender::Male)
                        .collect();
                    let females: Vec<&OwnedPal> = self
                        .owned
                        .iter()
                        .filter(|p| p.species == name && p.gender == Gender::Female)
                        .collect();
                    let mut best_mask = 0u64;
                    for m in &males {
                        for f in &females {
                            let mask = self.mask_of(&m.passives) | self.mask_of(&f.passives);
                            if mask.count_ones() > best_mask.count_ones() {
                                best_mask = mask;
                            }
                        }
                    }
                    alts.push(Alternative {
                        parents: (name.clone(), name.clone()),
                        kind: BreedKind::Formula,
                        cost: 1,
                        depth: 1,
                        covered: best_mask.count_ones(),
                    });
                }
            }
            for (a, b, _outcome) in self.db.parents_of(&name) {
                if a == b {
                    continue;
                }
                // 跳过以该物种自身为亲本的自指组合（对规划展示无意义）
                if a == s || b == s {
                    continue;
                }
                // 双亲可用性：优先配种条目，否则已持有条目
                let (Some(ea), Some(eb)) = (
                    self.entries[a][1].as_ref().or(self.entries[a][0].as_ref()),
                    self.entries[b][1].as_ref().or(self.entries[b][0].as_ref()),
                ) else {
                    continue;
                };
                // 性别可行性（含性别唯一组合的雌性指定）
                if !(ea.male || eb.male) || !(ea.female || eb.female) {
                    continue;
                }
                let pa = &self.db.pals[a].internal_name;
                let pb = &self.db.pals[b].internal_name;
                let (kind, gender_ok) = match self.db.breed(pa, pb) {
                    Some(BreedOutcome::Normal(_)) => (
                        if self.db.is_unique_pair(pa, pb) {
                            BreedKind::Unique
                        } else {
                            BreedKind::Formula
                        },
                        true,
                    ),
                    Some(BreedOutcome::GenderDependent {
                        if_p1_female,
                        if_p2_female,
                    }) => {
                        if if_p1_female == s {
                            (BreedKind::GenderUnique, ea.female)
                        } else if if_p2_female == s {
                            (BreedKind::GenderUnique, eb.female)
                        } else {
                            continue;
                        }
                    }
                    None => continue,
                };
                if !gender_ok {
                    continue;
                }
                alts.push(Alternative {
                    parents: if pa <= pb {
                        (pa.clone(), pb.clone())
                    } else {
                        (pb.clone(), pa.clone())
                    },
                    kind,
                    cost: ea.cost + eb.cost + 1,
                    depth: ea.depth.max(eb.depth) + 1,
                    covered: (ea.passive_mask | eb.passive_mask).count_ones(),
                });
            }
            alts.sort_by_key(|alt| (alt.cost, alt.depth, std::cmp::Reverse(alt.covered)));
            alts.truncate(12);
            if !alts.is_empty() {
                out.insert(name, alts);
            }
        }
        out
    }

    /// 由最终条目重建 Plan（统计配种次数、世代、用到的已持有帕鲁）。
    fn into_plan(self, target: usize, entry: &Entry) -> Plan {
        let alternatives = self.collect_alternatives(target);
        let root_ref = (target, entry.via.is_some());
        let root = self.build_node(root_ref, None, &mut Vec::new());
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
            alternatives,
        }
    }

    /// 已持有目标帕鲁时的平凡结果（0 次配种）。
    fn trivial_plan(self, target: usize, alternatives: HashMap<String, Vec<Alternative>>) -> Plan {
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
            alternatives,
        }
    }
}

/// 计算从已持有帕鲁到目标物种的最优配种路径（无钉选）。
#[allow(dead_code)] // 测试使用；生产代码走 plan_with_pins
pub fn plan(
    db: &BreedingDB,
    owned: &[OwnedPal],
    target: &str,
    desired_passives: &[String],
) -> Result<Plan, PlanError> {
    plan_with_pins(db, owned, target, desired_passives, &HashMap::new())
}

/// 带钉选亲本对（物种 → 无序亲本对）的规划。
///
/// 被动语义：已持有目标但单只未覆盖全部期望被动时视为"未达成"，
/// 转而求配种路径。两级求解：先求"最少配种次数"的路径，若其血统能覆盖
/// 全部期望被动则直接采用；否则再求"覆盖优先"的路径（可接受更多次数），
/// 两者取覆盖更好者（平局取次数少者）。覆盖优先为启发式，非严格最优。
/// 钉选在树重建阶段局部生效：仅替换被钉选节点的亲本对，不影响搜索与其余结构；
/// 不可行或成环的钉选自动忽略。
pub fn plan_with_pins(
    db: &BreedingDB,
    owned: &[OwnedPal],
    target: &str,
    desired_passives: &[String],
    pins: &HashMap<String, (String, String)>,
) -> Result<Plan, PlanError> {
    plan_inner(db, owned, target, desired_passives, pins)
}

fn plan_inner(
    db: &BreedingDB,
    owned: &[OwnedPal],
    target: &str,
    desired_passives: &[String],
    pins: &HashMap<String, (String, String)>,
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

    // 目标一律求配种路径（即便已持有——作为目标即表示要配种获得）。
    // 仅在完全无法配种获得时，才回退展示已持有个体。

    // 池位索引：覆盖所有已持有帕鲁出现过的词条（含非期望）
    let pidx: HashMap<String, u32> = {
        let mut m = HashMap::new();
        for p in owned {
            for ps in &p.passives {
                if !m.contains_key(ps) {
                    m.insert(ps.clone(), m.len() as u32);
                }
            }
        }
        m
    };

    // A：最少配种次数优先
    let mut a = Planner::new(db, owned, &desired, &pidx, pins, target_idx, false);
    let best_a = a.run();
    let full_of = |mask: u64| mask.count_ones() as usize == desired.len();
    let mask_of = |passives: &[String]| -> u64 {
        desired.iter().enumerate().fold(0u64, |m, (i, d)| {
            if passives.contains(d) {
                m | (1 << i)
            } else {
                m
            }
        })
    };
    let pool_of = |passives: &[String]| -> u128 {
        passives.iter().fold(0u128, |m, p| match pidx.get(p) {
            Some(&i) => m | (1u128 << i),
            None => m,
        })
    };

    // C：同种合并链——把多只已持有同种的被动逐步合并（X×X→X 反复进行）。
    // 选种取并集最大（平局取池更小）的异性对，之后按"新增覆盖多、垃圾词条少"
    // 贪心并入；配种产物性别按均可获得处理，因此链上每步都可行。
    let same_chain: Option<(Vec<u64>, u64, u128)> = {
        let inds: Vec<&OwnedPal> = owned
            .iter()
            .filter(|p| p.species == target && mask_of(&p.passives) > 0)
            .collect();
        if inds.len() < 2 {
            None
        } else {
            let mut seed: Option<(&OwnedPal, &OwnedPal, u64, u128)> = None;
            for x in &inds {
                for y in &inds {
                    if x.id >= y.id || x.gender == y.gender {
                        continue;
                    }
                    let m = mask_of(&x.passives) | mask_of(&y.passives);
                    let pool = pool_of(&x.passives) | pool_of(&y.passives);
                    let better = seed.is_none_or(|(_, _, bm, bp)| {
                        (m.count_ones(), std::cmp::Reverse(pool.count_ones()))
                            > (bm.count_ones(), std::cmp::Reverse(bp.count_ones()))
                    });
                    if better {
                        seed = Some((x, y, m, pool));
                    }
                }
            }
            seed.map(|(a, b, m, pool)| {
                let mut order = vec![a.id, b.id];
                let mut acc = m;
                let mut acc_pool = pool;
                let mut rest: Vec<(&OwnedPal, u32, u32)> = inds
                    .iter()
                    .copied()
                    .filter(|p| p.id != a.id && p.id != b.id)
                    .map(|p| {
                        let new_bits = (mask_of(&p.passives) & !acc).count_ones();
                        let junk = (pool_of(&p.passives) & !acc_pool).count_ones();
                        (p, new_bits, junk)
                    })
                    .collect();
                // 新增覆盖多者优先，平局取带入垃圾词条少者
                rest.sort_by_key(|(_, new_bits, junk)| {
                    (std::cmp::Reverse(*new_bits), *junk)
                });
                for (p, new_bits, _) in rest {
                    if new_bits > 0 {
                        acc |= mask_of(&p.passives);
                        acc_pool |= pool_of(&p.passives);
                        order.push(p.id);
                    }
                }
                (order, acc, acc_pool)
            })
        }
    };

    // B：覆盖优先（A 未全覆盖时才需要跑）
    let mut b_planner = None;
    let mut best_b = None;
    if !desired.is_empty() && best_a.as_ref().is_none_or(|e| !full_of(e.passive_mask)) {
        let mut b = Planner::new(db, owned, &desired, &pidx, pins, target_idx, true);
        best_b = b.run();
        b_planner = Some(b);
    }

    // 候选统一比较：全覆盖优先 → 覆盖更多 → 池更干净 → 总次数更少
    // key = (!full, Reverse(covered), pool_size, cost)，取最小
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    struct CandKey(u8, std::cmp::Reverse<u32>, u32, u32);
    let key = |full: bool, cov: u32, pool: u32, cost: u32| {
        CandKey(!full as u8, std::cmp::Reverse(cov), pool, cost)
    };

    let mut winner: Option<(CandKey, usize)> = None; // 0=A 1=B 2=C
    let consider = |k: CandKey, which: usize, winner: &mut Option<(CandKey, usize)>| {
        if winner.as_ref().is_none_or(|(wk, _)| k < *wk) {
            *winner = Some((k, which));
        }
    };
    if let Some(e) = &best_a {
        consider(
            key(
                full_of(e.passive_mask),
                e.passive_mask.count_ones(),
                e.pool.count_ones(),
                e.cost,
            ),
            0,
            &mut winner,
        );
    }
    if let Some(e) = &best_b {
        consider(
            key(
                full_of(e.passive_mask),
                e.passive_mask.count_ones(),
                e.pool.count_ones(),
                e.cost,
            ),
            1,
            &mut winner,
        );
    }
    if let Some((order, mask, pool)) = &same_chain {
        let cost = (order.len() - 1) as u32;
        consider(
            key(full_of(*mask), mask.count_ones(), pool.count_ones(), cost),
            2,
            &mut winner,
        );
    }

    match winner.map(|(_, w)| w) {
        Some(0) => {
            let e = best_a.unwrap();
            Ok(a.into_plan(target_idx, &e))
        }
        Some(1) => {
            let e = best_b.unwrap();
            Ok(b_planner.unwrap().into_plan(target_idx, &e))
        }
        Some(2) => {
            let (order, _, _) = same_chain.unwrap();
            let alternatives = a.collect_alternatives(target_idx);
            Ok(build_same_chain(owned, target, &order, alternatives))
        }
        _ => {
            // 没有可行配种路径：已持有（被动不符）则如实展示，否则报告不可达
            if owned.iter().any(|p| p.species == target) {
                return Ok(Planner::new(db, owned, &desired, &pidx, pins, target_idx, false)
                    .trivial_plan(target_idx, HashMap::new()));
            }
            Err(PlanError::Unreachable {
                target: target.to_string(),
                unique_parents: db.unique_parents_of(target),
            })
        }
    }
}

/// 把同种合并链（有序的 pal id 列表）构建为配种链树。
fn build_same_chain(
    owned: &[OwnedPal],
    target: &str,
    order: &[u64],
    alternatives: HashMap<String, Vec<Alternative>>,
) -> Plan {
    let pal = |id: u64| owned.iter().find(|p| p.id == id).expect("chain 中的 id 必然存在");
    let leaf = |p: &OwnedPal, need: Gender| PlanNode {
        species: target.to_string(),
        need_gender: Some(need),
        source: PlanSource::Owned { pal_id: p.id },
    };
    let opposite = |g: Gender| match g {
        Gender::Male => Gender::Female,
        Gender::Female => Gender::Male,
    };
    let a = pal(order[0]);
    let b = pal(order[1]);
    let mut node = PlanNode {
        species: target.to_string(),
        need_gender: None,
        source: PlanSource::Bred {
            kind: BreedKind::Formula,
            p1: Box::new(leaf(a, a.gender)),
            p2: Box::new(leaf(b, b.gender)),
        },
    };
    for &id in &order[2..] {
        let ind = pal(id);
        node = PlanNode {
            species: target.to_string(),
            need_gender: None,
            source: PlanSource::Bred {
                kind: BreedKind::Formula,
                // 配种产物性别任选 → 取与并入个体相反的性别
                p1: Box::new(PlanNode {
                    need_gender: Some(opposite(ind.gender)),
                    ..node
                }),
                p2: Box::new(leaf(ind, ind.gender)),
            },
        };
    }
    Plan {
        root: node,
        total_breedings: (order.len() - 1) as u32,
        generations: (order.len() - 1) as u32,
        used_owned: {
            let mut v = order.to_vec();
            v.sort_unstable();
            v
        },
        alternatives,
    }
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
            group: PalGroup::Box,
            is_boss: false,
            favorite: 0,
            nickname: None,
            is_lucky: false,
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

    /// 钉选：强制使用指定亲本对，即使它不是默认最优；备选枚举应列出全部可行组合。
    #[test]
    fn pin_forces_specific_parent_pair() {
        // 数据：B×C 或 C×D → E（B×D 双雌不可行）；全部 cost 1
        let db = sample_db();
        let owned = vec![
            owned(1, "B", Gender::Female, &[]),
            owned(2, "C", Gender::Male, &[]),
            owned(3, "D", Gender::Female, &[]),
        ];
        let mut pins = HashMap::new();
        pins.insert("E".to_string(), ("C".to_string(), "D".to_string()));
        let plan = plan_with_pins(&db, &owned, "E", &[], &pins).unwrap();
        assert_eq!(plan.total_breedings, 1);
        assert!(matches!(
            child_by_species(&plan.root, "C").source,
            PlanSource::Owned { pal_id: 2 }
        ));
        assert!(matches!(
            child_by_species(&plan.root, "D").source,
            PlanSource::Owned { pal_id: 3 }
        ));
        let alts = &plan.alternatives["E"];
        assert_eq!(alts.len(), 2);
        assert!(
            alts.iter()
                .any(|a| a.parents == ("B".to_string(), "C".to_string()))
        );
        assert!(
            alts.iter()
                .any(|a| a.parents == ("C".to_string(), "D".to_string()))
        );
    }

    /// 同种配种：期望被动分散在两只已持有同种身上时，自配优于公式路径。
    #[test]
    fn same_species_consolidates_passives() {
        // A(100)×B(200) → X(150)（公式）；X 已持有雌雄各一，被动互补
        let db = BreedingDB::new(
            vec![
                pal("A", 100, 1, false),
                pal("B", 200, 2, false),
                pal("X", 150, 3, true),
            ],
            vec![],
        );
        let owned = vec![
            owned(1, "A", Gender::Male, &[]),
            owned(2, "B", Gender::Female, &[]),
            owned(3, "X", Gender::Male, &["lucky"]),
            owned(4, "X", Gender::Female, &["brave"]),
        ];
        let plan = plan(
            &db,
            &owned,
            "X",
            &["lucky".to_string(), "brave".to_string()],
        )
        .unwrap();
        assert_eq!(plan.total_breedings, 1);
        let PlanSource::Bred { p1, p2, .. } = &plan.root.source else {
            panic!();
        };
        assert_eq!(p1.species, "X");
        assert_eq!(p2.species, "X");
        assert!(plan.used_owned.contains(&3) && plan.used_owned.contains(&4));
        // 备选列表应包含同种组合
        assert!(
            plan.alternatives["X"]
                .iter()
                .any(|a| a.parents.0 == "X" && a.parents.1 == "X")
        );
    }

    /// 钉选失效（该组合不产出目标）→ 自动忽略，按最优处理。
    #[test]
    fn stale_pin_is_ignored() {
        let db = sample_db();
        let owned = vec![
            owned(1, "B", Gender::Female, &[]),
            owned(2, "C", Gender::Male, &[]),
        ];
        let mut pins = HashMap::new();
        // A×B 产出 C 而不是 E，该钉选不可行 → 忽略后正常规划
        pins.insert("E".to_string(), ("A".to_string(), "B".to_string()));
        let plan = plan_with_pins(&db, &owned, "E", &[], &pins).unwrap();
        assert_eq!(plan.total_breedings, 1);
    }

    /// 同种合并链：三只各带一个期望被动的同种 → 两次自配收满。
    #[test]
    fn same_species_chain_merges_three_pals() {
        let db = BreedingDB::new(vec![pal("X", 150, 1, true)], vec![]);
        let owned = vec![
            owned(1, "X", Gender::Male, &["lucky"]),
            owned(2, "X", Gender::Female, &["brave"]),
            owned(3, "X", Gender::Male, &["swift"]),
        ];
        let plan = plan(
            &db,
            &owned,
            "X",
            &["lucky".to_string(), "brave".to_string(), "swift".to_string()],
        )
        .unwrap();
        assert_eq!(plan.total_breedings, 2);
        assert_eq!(plan.generations, 2);
        assert_eq!(plan.used_owned, vec![1, 2, 3]);
        // 链末端（根的亲本一）应为配种节点
        let PlanSource::Bred { p1, p2, .. } = &plan.root.source else {
            panic!();
        };
        assert!(matches!(p1.source, PlanSource::Bred { .. }));
        assert!(matches!(p2.source, PlanSource::Owned { .. }));
    }

    /// 池感知：覆盖相同时，优先选择垃圾词条更少（继承池更干净）的亲本路径。
    #[test]
    fn cleaner_pool_wins_on_tie() {
        // P1(190)×P2(210) → T(200)，P3(190)×P4(210) → T(200)，同为 cost 1 全覆盖
        // P1 只带 lucky（池 1），P3 带 lucky+垃圾（池 2）→ 应选 P1 路径
        let db = BreedingDB::new(
            vec![
                pal("P1", 190, 1, false),
                pal("P2", 210, 2, false),
                pal("P3", 190, 3, false),
                pal("P4", 210, 4, false),
                pal("T", 200, 5, true),
            ],
            vec![],
        );
        let owned = vec![
            owned(1, "P1", Gender::Male, &["lucky"]),
            owned(2, "P2", Gender::Female, &[]),
            owned(3, "P3", Gender::Male, &["lucky", "clumsy"]),
            owned(4, "P4", Gender::Female, &[]),
        ];
        let plan = plan(&db, &owned, "T", &["lucky".to_string()]).unwrap();
        assert!(plan.used_owned.contains(&1));
        assert!(!plan.used_owned.contains(&3));
    }
}
