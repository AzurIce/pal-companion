use super::{Alternative, BreedKind, Gender, OwnedPal, Plan, PlanError, PlanNode, PlanSource};
use crate::breeding::{BreedOutcome, BreedingDB};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

type BitSet = Box<[u64]>;

#[derive(Clone)]
struct State {
    species: usize,
    mask: u8,
    pool: BitSet,
    cost: u32,
    depth: u32,
    satisfied_pins: BitSet,
    violated_pins: BitSet,
    source: Source,
}

#[derive(Clone)]
enum Source {
    Owned {
        pal_id: u64,
        gender: Gender,
    },
    Bred {
        kind: BreedKind,
        p1: Rc<State>,
        p2: Rc<State>,
        g1: Gender,
        g2: Gender,
    },
}

impl State {
    fn can_be(&self, gender: Gender) -> bool {
        match self.source {
            Source::Owned { gender: actual, .. } => actual == gender,
            Source::Bred { .. } => true,
        }
    }

    fn is_owned(&self) -> bool {
        matches!(self.source, Source::Owned { .. })
    }

    fn owned_id(&self) -> Option<u64> {
        match self.source {
            Source::Owned { pal_id, .. } => Some(pal_id),
            Source::Bred { .. } => None,
        }
    }

    fn source_key(&self) -> u64 {
        match self.source {
            Source::Owned { pal_id, .. } => pal_id,
            Source::Bred { .. } => u64::MAX,
        }
    }
}

struct Solver<'a> {
    db: &'a BreedingDB,
    desired: Vec<String>,
    pidx: HashMap<String, u32>,
    pins: &'a HashMap<String, (String, String)>,
    pin_bits: HashMap<usize, usize>,
    all_pins: BitSet,
    states: Vec<Vec<Rc<State>>>,
}

impl<'a> Solver<'a> {
    fn new(
        db: &'a BreedingDB,
        owned: &'a [OwnedPal],
        desired_passives: &[String],
        pins: &'a HashMap<String, (String, String)>,
    ) -> Self {
        let mut desired = Vec::new();
        for passive in desired_passives.iter().take(4) {
            if !desired.contains(passive) {
                desired.push(passive.clone());
            }
        }
        let mut pidx = HashMap::new();
        for pal in owned
            .iter()
            .filter(|pal| db.index_of(&pal.species).is_some())
        {
            for passive in &pal.passives {
                let next = pidx.len() as u32;
                pidx.entry(passive.clone()).or_insert(next);
            }
        }
        let pin_bits: HashMap<usize, usize> = pins
            .keys()
            .filter_map(|species| db.index_of(species))
            .enumerate()
            .map(|(bit, species)| (species, bit))
            .collect();
        let mut all_pins = empty_bits(pin_bits.len());
        for bit in pin_bits.values() {
            bit_insert(&mut all_pins, *bit);
        }
        let mut solver = Self {
            db,
            desired,
            pidx,
            pins,
            pin_bits,
            all_pins,
            states: vec![Vec::new(); db.pals.len()],
        };
        for pal in owned {
            let Some(species) = db.index_of(&pal.species) else {
                continue;
            };
            let state = Rc::new(State {
                species,
                mask: solver.mask_of(&pal.passives),
                pool: solver.pool_of(&pal.passives),
                cost: 0,
                depth: 0,
                satisfied_pins: empty_bits(solver.pin_bits.len()),
                violated_pins: empty_bits(solver.pin_bits.len()),
                source: Source::Owned {
                    pal_id: pal.id,
                    gender: pal.gender,
                },
            });
            solver.insert(state);
        }
        solver
    }

    fn mask_of(&self, passives: &[String]) -> u8 {
        self.desired
            .iter()
            .enumerate()
            .fold(0, |mask, (i, desired)| {
                if passives.contains(desired) {
                    mask | (1 << i)
                } else {
                    mask
                }
            })
    }

    fn pool_of(&self, passives: &[String]) -> BitSet {
        let mut pool = empty_bits(self.pidx.len());
        for passive in passives {
            if let Some(bit) = self.pidx.get(passive) {
                bit_insert(&mut pool, *bit as usize);
            }
        }
        pool
    }

    fn dominates(a: &State, b: &State) -> bool {
        (a.mask | b.mask) == a.mask
            && bit_is_subset(&a.pool, &b.pool)
            && a.cost <= b.cost
            && a.depth <= b.depth
            && (a.can_be(Gender::Male) as u8) >= (b.can_be(Gender::Male) as u8)
            && (a.can_be(Gender::Female) as u8) >= (b.can_be(Gender::Female) as u8)
    }

    fn rank(state: &State) -> (u32, u32, u32, u32, u32, u32, u64) {
        (
            bit_count(&state.violated_pins),
            u32::MAX - bit_count(&state.satisfied_pins),
            u8::MAX as u32 - state.mask.count_ones(),
            bit_count(&state.pool),
            state.cost,
            state.depth,
            state.source_key(),
        )
    }

    fn insert(&mut self, state: Rc<State>) -> bool {
        let bucket = &mut self.states[state.species];
        if bucket.iter().any(|existing| {
            existing.satisfied_pins == state.satisfied_pins
                && existing.violated_pins == state.violated_pins
                && Self::dominates(existing, &state)
        }) {
            return false;
        }
        bucket.retain(|existing| {
            existing.satisfied_pins != state.satisfied_pins
                || existing.violated_pins != state.violated_pins
                || !Self::dominates(&state, existing)
        });
        bucket.push(state.clone());
        bucket.sort_by_key(|state| Self::rank(state));
        bucket.iter().any(|candidate| Rc::ptr_eq(candidate, &state))
    }

    fn pin_allows(&self, child: usize, a: usize, b: usize) -> bool {
        let child_name = &self.db.pals[child].internal_name;
        let Some((pa, pb)) = self.pins.get(child_name) else {
            return true;
        };
        let a = &self.db.pals[a].internal_name;
        let b = &self.db.pals[b].internal_name;
        (a == pa && b == pb) || (a == pb && b == pa)
    }

    fn outcomes(&self, a: &Rc<State>, b: &Rc<State>) -> Vec<(usize, BreedKind, Gender, Gender)> {
        let pa = &self.db.pals[a.species].internal_name;
        let pb = &self.db.pals[b.species].internal_name;
        let Some(outcome) = self.db.breed(pa, pb) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match outcome {
            BreedOutcome::Normal(child) => {
                let kind = if self.db.is_unique_pair(pa, pb) {
                    BreedKind::Unique
                } else {
                    BreedKind::Formula
                };
                if a.can_be(Gender::Male) && b.can_be(Gender::Female) {
                    out.push((child, kind, Gender::Male, Gender::Female));
                }
                if a.can_be(Gender::Female) && b.can_be(Gender::Male) {
                    out.push((child, kind, Gender::Female, Gender::Male));
                }
            }
            BreedOutcome::GenderDependent {
                if_p1_female,
                if_p2_female,
            } => {
                if a.can_be(Gender::Female) && b.can_be(Gender::Male) {
                    out.push((
                        if_p1_female,
                        BreedKind::GenderUnique,
                        Gender::Female,
                        Gender::Male,
                    ));
                }
                if a.can_be(Gender::Male) && b.can_be(Gender::Female) {
                    out.push((
                        if_p2_female,
                        BreedKind::GenderUnique,
                        Gender::Male,
                        Gender::Female,
                    ));
                }
            }
        }
        out
    }

    fn expand(&mut self, target: usize) {
        let mut queue: VecDeque<Rc<State>> = self
            .states
            .iter()
            .flat_map(|states| states.iter().cloned())
            .collect();
        while let Some(a) = queue.pop_front() {
            // a 可能在入队后被更优状态支配并从前沿移除，此时无需继续扩展。
            if !self.states[a.species]
                .iter()
                .any(|state| Rc::ptr_eq(state, &a))
            {
                continue;
            }
            let ideal_target_cost = self.states[target]
                .iter()
                .filter(|state| {
                    state.mask.count_ones() as usize == self.desired.len()
                        && bit_count(&state.pool) as usize == self.desired.len()
                        && !state.is_owned()
                        && self.pins_satisfied(state)
                })
                .map(|state| state.cost)
                .min();
            // 目标按产品语义必须经至少一次配种获得；cost=1 且全覆盖、无垃圾
            // 已达到所有排序维度的理论下界，后续状态不可能改善结果。
            if ideal_target_cost == Some(1) {
                break;
            }
            // 子代成本严格大于任一亲本。已有“全覆盖且无垃圾被动”的目标后，
            // 成本不小于它的状态不可能再改善最终排序。
            if ideal_target_cost.is_some_and(|best| a.cost >= best) {
                continue;
            }
            let partners: Vec<Rc<State>> = self
                .states
                .iter()
                .flat_map(|states| states.iter().cloned())
                .collect();
            for b in partners {
                // 同一只已持有个体不能和自己配种；配种状态则可以重复生产
                // 两只不同性别的副本后自配（成本自然计为两棵子树之和）。
                if a.owned_id().is_some() && a.owned_id() == b.owned_id() {
                    continue;
                }
                for (child, kind, g1, g2) in self.outcomes(&a, &b) {
                    let state = Rc::new(State {
                        species: child,
                        mask: a.mask | b.mask,
                        pool: bit_union(&a.pool, &b.pool),
                        cost: a.cost + b.cost + 1,
                        depth: a.depth.max(b.depth) + 1,
                        satisfied_pins: {
                            let mut inherited = bit_union(&a.satisfied_pins, &b.satisfied_pins);
                            if self.pin_allows(child, a.species, b.species) {
                                if let Some(bit) = self.pin_bits.get(&child) {
                                    bit_insert(&mut inherited, *bit);
                                }
                            }
                            inherited
                        },
                        violated_pins: {
                            let mut inherited = bit_union(&a.violated_pins, &b.violated_pins);
                            if !self.pin_allows(child, a.species, b.species) {
                                if let Some(bit) = self.pin_bits.get(&child) {
                                    bit_insert(&mut inherited, *bit);
                                }
                            }
                            inherited
                        },
                        source: Source::Bred {
                            kind,
                            p1: a.clone(),
                            p2: b.clone(),
                            g1,
                            g2,
                        },
                    });
                    if self.insert(state.clone()) {
                        queue.push_back(state);
                    }
                }
            }
        }
    }

    fn build_node(&self, state: &State, need_gender: Option<Gender>) -> PlanNode {
        let species = self.db.pals[state.species].internal_name.clone();
        let source = match &state.source {
            Source::Owned { pal_id, .. } => PlanSource::Owned { pal_id: *pal_id },
            Source::Bred {
                kind,
                p1,
                p2,
                g1,
                g2,
            } => PlanSource::Bred {
                kind: *kind,
                p1: Box::new(self.build_node(p1, Some(*g1))),
                p2: Box::new(self.build_node(p2, Some(*g2))),
            },
        };
        PlanNode {
            species,
            need_gender,
            source,
        }
    }

    fn pins_satisfied(&self, state: &State) -> bool {
        state.satisfied_pins == self.all_pins && bit_is_empty(&state.violated_pins)
    }

    fn alternatives(&self, root: &Rc<State>) -> HashMap<String, Vec<Alternative>> {
        let mut species = HashSet::new();
        collect_bred_species(root, &mut species);
        let mut result = HashMap::new();
        for child in species {
            let child_name = self.db.pals[child].internal_name.clone();
            let mut by_pair: HashMap<(String, String), Alternative> = HashMap::new();
            for (a_idx, b_idx, outcome) in self.db.parents_of(&child_name) {
                if (a_idx == child || b_idx == child) && (a_idx != b_idx || child != root.species) {
                    continue;
                }
                let a_name = self.db.pals[a_idx].internal_name.clone();
                let b_name = self.db.pals[b_idx].internal_name.clone();
                let parents = if a_name <= b_name {
                    (a_name, b_name)
                } else {
                    (b_name, a_name)
                };
                let Some((mut kind, required)) = alternative_kind(outcome, child) else {
                    continue;
                };
                if kind == BreedKind::Formula
                    && self.db.is_unique_pair(
                        &self.db.pals[a_idx].internal_name,
                        &self.db.pals[b_idx].internal_name,
                    )
                {
                    kind = BreedKind::Unique;
                }
                let mut best: Option<Alternative> = None;
                for a in &self.states[a_idx] {
                    for b in &self.states[b_idx] {
                        if a.owned_id().is_some() && a.owned_id() == b.owned_id() {
                            continue;
                        }
                        let gender_ok = match required {
                            None => {
                                (a.can_be(Gender::Male) && b.can_be(Gender::Female))
                                    || (a.can_be(Gender::Female) && b.can_be(Gender::Male))
                            }
                            Some((ga, gb)) => a.can_be(ga) && b.can_be(gb),
                        };
                        if !gender_ok {
                            continue;
                        }
                        let alt = Alternative {
                            parents: parents.clone(),
                            kind,
                            cost: a.cost + b.cost + 1,
                            depth: a.depth.max(b.depth) + 1,
                            covered: (a.mask | b.mask).count_ones(),
                        };
                        if best
                            .as_ref()
                            .is_none_or(|old| alternative_cmp(&alt, old) == Ordering::Less)
                        {
                            best = Some(alt);
                        }
                    }
                }
                if let Some(alt) = best {
                    by_pair
                        .entry(parents)
                        .and_modify(|old| {
                            if alternative_cmp(&alt, old) == Ordering::Less {
                                *old = alt.clone();
                            }
                        })
                        .or_insert(alt);
                }
            }
            let mut alternatives: Vec<_> = by_pair.into_values().collect();
            alternatives.sort_by(alternative_cmp);
            let root_is_self_bred = child == root.species
                && matches!(&root.source, Source::Bred { p1, p2, .. }
                    if p1.species == child && p2.species == child);
            if !root_is_self_bred {
                alternatives
                    .retain(|alt| alt.parents.0 != child_name || alt.parents.1 != child_name);
            }
            alternatives.truncate(12);
            if !alternatives.is_empty() {
                result.insert(child_name, alternatives);
            }
        }
        result
    }
}

fn alternative_kind(
    outcome: BreedOutcome,
    child: usize,
) -> Option<(BreedKind, Option<(Gender, Gender)>)> {
    match outcome {
        BreedOutcome::Normal(actual) if actual == child => Some((BreedKind::Formula, None)),
        BreedOutcome::GenderDependent {
            if_p1_female,
            if_p2_female,
        } => {
            if if_p1_female == child {
                Some((
                    BreedKind::GenderUnique,
                    Some((Gender::Female, Gender::Male)),
                ))
            } else if if_p2_female == child {
                Some((
                    BreedKind::GenderUnique,
                    Some((Gender::Male, Gender::Female)),
                ))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn collect_bred_species(state: &Rc<State>, out: &mut HashSet<usize>) {
    if let Source::Bred { p1, p2, .. } = &state.source {
        if out.insert(state.species) {
            collect_bred_species(p1, out);
            collect_bred_species(p2, out);
        }
    }
}

fn empty_bits(bit_count: usize) -> BitSet {
    vec![0; bit_count.div_ceil(64)].into_boxed_slice()
}

fn bit_insert(set: &mut BitSet, bit: usize) {
    set[bit / 64] |= 1u64 << (bit % 64);
}

fn bit_is_empty(set: &BitSet) -> bool {
    set.iter().all(|word| *word == 0)
}

fn bit_count(set: &BitSet) -> u32 {
    set.iter().map(|word| word.count_ones()).sum()
}

fn bit_union(a: &BitSet, b: &BitSet) -> BitSet {
    a.iter()
        .zip(b.iter())
        .map(|(a, b)| a | b)
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn bit_is_subset(a: &BitSet, b: &BitSet) -> bool {
    a.iter().zip(b.iter()).all(|(a, b)| a & !b == 0)
}

fn alternative_cmp(a: &Alternative, b: &Alternative) -> Ordering {
    (a.cost, a.depth, std::cmp::Reverse(a.covered), &a.parents).cmp(&(
        b.cost,
        b.depth,
        std::cmp::Reverse(b.covered),
        &b.parents,
    ))
}

fn candidate_rank(state: &State, desired_count: usize) -> (u8, u32, u32, u32, u32, u64) {
    let full = state.mask.count_ones() as usize == desired_count;
    (
        !full as u8,
        u8::MAX as u32 - state.mask.count_ones(),
        bit_count(&state.pool),
        state.cost,
        state.depth,
        state.source_key(),
    )
}

pub(super) fn solve(
    db: &BreedingDB,
    owned: &[OwnedPal],
    target: &str,
    desired_passives: &[String],
    pins: &HashMap<String, (String, String)>,
) -> Result<Plan, PlanError> {
    let Some(target_idx) = db.index_of(target) else {
        return Err(PlanError::UnknownSpecies(target.to_string()));
    };
    let mut solver = Solver::new(db, owned, desired_passives, pins);
    solver.expand(target_idx);
    let desired_count = solver.desired.len();
    let target_states = &solver.states[target_idx];
    if pins.is_empty() {
        if let Some(best_owned) = target_states
            .iter()
            .filter(|state| state.is_owned() && state.mask.count_ones() as usize == desired_count)
            .min_by_key(|state| candidate_rank(state, desired_count))
            .cloned()
        {
            return Ok(solver.into_plan(best_owned));
        }
    }
    let compliant: Vec<_> = target_states
        .iter()
        .filter(|state| {
            solver.pins_satisfied(state)
                && (!state.is_owned() || state.mask.count_ones() as usize == desired_count)
        })
        .collect();
    let candidates: Vec<_> = if !compliant.is_empty() {
        compliant
    } else {
        let fallback: Vec<_> = target_states
            .iter()
            .filter(|state| !state.is_owned() || state.mask.count_ones() as usize == desired_count)
            .collect();
        if fallback.is_empty() {
            target_states.iter().collect()
        } else {
            fallback
        }
    };
    let Some(best) = candidates
        .into_iter()
        .min_by_key(|state| candidate_rank(state, desired_count))
        .cloned()
    else {
        return Err(PlanError::Unreachable {
            target: target.to_string(),
            unique_parents: db.unique_parents_of(target),
        });
    };
    Ok(solver.into_plan(best))
}

impl Solver<'_> {
    fn into_plan(&self, best: Rc<State>) -> Plan {
        let root = self.build_node(&best, None);
        let mut breedings = 0;
        let mut used_owned = Vec::new();
        let generations = super::collect_stats(&root, &mut breedings, &mut used_owned);
        used_owned.sort_unstable();
        used_owned.dedup();
        let alternatives = self.alternatives(&best);
        Plan {
            root,
            total_breedings: breedings,
            generations,
            used_owned,
            alternatives,
        }
    }
}
