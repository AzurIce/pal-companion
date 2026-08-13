//! 配种路径的图布局：相同子树折叠为 DAG 节点（复用），已持有在左、目标在右。
//!
//! 折叠规则：两个节点可合并当且仅当结构键相同——
//! 已持有叶子按 (pal_id)，配种节点按 (物种, 需求性别, 组合类型, 左右子树键)。
//! 因此不同性别要求或不同已持有个体（如不同被动）不会被折叠。
//!
//! 布局：列 = 到叶子的最长路径高度；列内 y 初值为"叶子按 DFS 序占槽、内部节点
//! 取子节点均值"，再做 8 轮重心松弛 + 列内最小间距约束（保持列重心）。

use crate::planner::{BreedKind, Gender, PlanNode, PlanSource};
use dioxus_flow::{FlowEdge, FlowNode, NodeId, Point, Size};
use std::collections::{HashMap, HashSet};

pub const NODE_W: f64 = 200.0;
pub const NODE_H: f64 = 100.0;
pub const COL_GAP: f64 = 92.0;
pub const ROW_GAP: f64 = 30.0;
const PITCH: f64 = NODE_H + ROW_GAP;
const RELAX_ITERS: usize = 8;

#[derive(Clone, PartialEq)]
pub struct GraphData {
    pub nodes: Vec<FlowNode>,
    pub edges: Vec<FlowEdge>,
    /// 节点 id → 业务数据（被折叠的多个树节点共享同一代表）
    pub info: HashMap<String, PlanNode>,
    /// 根（目标）节点 id
    pub root_id: String,
    /// 实际需要执行的配种次数（按原始配种树计；图形折叠不代表少做一次配种）
    pub bred_count: usize,
    /// 实际用到的已持有帕鲁 id
    pub used_owned: Vec<u64>,
}

/// 子树结构键（折叠判据）
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Owned(u64),
    Bred {
        species: String,
        need: Option<Gender>,
        kind: BreedKind,
        a: Box<Key>,
        b: Box<Key>,
    },
}

fn key_of(node: &PlanNode) -> Key {
    match &node.source {
        PlanSource::Owned { pal_id } => Key::Owned(*pal_id),
        PlanSource::Bred { kind, p1, p2 } => Key::Bred {
            species: node.species.clone(),
            need: node.need_gender,
            kind: *kind,
            a: Box::new(key_of(p1)),
            b: Box::new(key_of(p2)),
        },
    }
}

struct Dag {
    /// id → 代表 PlanNode
    nodes: Vec<PlanNode>,
    /// (ingredient, product)，均有序去重
    edge_set: HashSet<(usize, usize)>,
    root: usize,
    key_map: HashMap<Key, usize>,
}

impl Dag {
    fn build(root: &PlanNode) -> Self {
        let mut dag = Dag {
            nodes: Vec::new(),
            edge_set: HashSet::new(),
            root: 0,
            key_map: HashMap::new(),
        };
        dag.root = dag.intern(root);
        dag
    }

    fn intern(&mut self, node: &PlanNode) -> usize {
        let key = key_of(node);
        if let Some(&id) = self.key_map.get(&key) {
            return id;
        }
        let id = self.nodes.len();
        self.nodes.push(node.clone());
        self.key_map.insert(key, id);
        if let PlanSource::Bred { kind, p1, p2 } = &node.source {
            let a = self.intern(p1);
            let b = self.intern(p2);
            self.edge_set.insert((a, id));
            self.edge_set.insert((b, id));
            let _ = kind;
        }
        id
    }
}

pub fn layout_plan(root: &PlanNode) -> GraphData {
    let dag = Dag::build(root);
    let n = dag.nodes.len();
    let edges: Vec<(usize, usize)> = dag.edge_set.iter().copied().collect();

    // 邻接表
    let mut children_of: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut parents_of: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in &edges {
        children_of[a].push(b);
        parents_of[b].push(a);
    }

    // 高度（到叶子的最长路径）：叶子=0
    let mut height = vec![usize::MAX; n];
    fn compute_height(
        id: usize,
        dag: &Dag,
        parents_of: &[Vec<usize>],
        height: &mut [usize],
    ) -> usize {
        if height[id] != usize::MAX {
            return height[id];
        }
        let h = if matches!(dag.nodes[id].source, PlanSource::Owned { .. }) {
            0
        } else {
            parents_of[id]
                .iter()
                .map(|&p| compute_height(p, dag, parents_of, height))
                .max()
                .unwrap_or(0)
                + 1
        };
        height[id] = h;
        h
    }
    for id in 0..n {
        compute_height(id, &dag, &parents_of, &mut height);
    }

    // 叶子 DFS 序占槽
    let mut leaf_slot: HashMap<usize, usize> = HashMap::new();
    let mut slot_count = 0usize;
    fn assign_slots(
        id: usize,
        dag: &Dag,
        parents_of: &[Vec<usize>],
        leaf_slot: &mut HashMap<usize, usize>,
        slot_count: &mut usize,
    ) {
        if matches!(dag.nodes[id].source, PlanSource::Owned { .. }) {
            if !leaf_slot.contains_key(&id) {
                leaf_slot.insert(id, *slot_count);
                *slot_count += 1;
            }
            return;
        }
        for &p in &parents_of[id] {
            assign_slots(p, dag, parents_of, leaf_slot, slot_count);
        }
    }
    assign_slots(dag.root, &dag, &parents_of, &mut leaf_slot, &mut slot_count);

    // y 初值：叶子按槽位，内部节点按高度升序取子节点均值
    let mut y = vec![0.0f64; n];
    for (&id, &slot) in &leaf_slot {
        y[id] = slot as f64 * PITCH + NODE_H / 2.0;
    }
    let mut by_height: Vec<usize> = (0..n).collect();
    by_height.sort_by_key(|&id| height[id]);
    for &id in &by_height {
        if leaf_slot.contains_key(&id) {
            continue;
        }
        let sum: f64 = parents_of[id].iter().map(|&p| y[p]).sum();
        y[id] = sum / parents_of[id].len().max(1) as f64;
    }

    // 重心松弛 + 列内最小间距（保持列重心）
    for _ in 0..RELAX_ITERS {
        for &id in &by_height {
            let mut neighbors: Vec<f64> = parents_of[id].iter().map(|&p| y[p]).collect();
            neighbors.extend(children_of[id].iter().map(|&c| y[c]));
            if neighbors.is_empty() {
                continue;
            }
            let mean: f64 = neighbors.iter().sum::<f64>() / neighbors.len() as f64;
            y[id] = 0.55 * y[id] + 0.45 * mean;
        }
        // 按列整理：排序后推开重叠，再整体平移保持列均值
        let mut columns: HashMap<usize, Vec<usize>> = HashMap::new();
        for id in 0..n {
            columns.entry(height[id]).or_default().push(id);
        }
        for (_, mut col) in columns {
            col.sort_by(|&a, &b| y[a].partial_cmp(&y[b]).unwrap());
            let mean_before: f64 = col.iter().map(|&id| y[id]).sum::<f64>() / col.len() as f64;
            for i in 1..col.len() {
                let min_y = y[col[i - 1]] + PITCH;
                if y[col[i]] < min_y {
                    y[col[i]] = min_y;
                }
            }
            let mean_after: f64 = col.iter().map(|&id| y[id]).sum::<f64>() / col.len() as f64;
            let drift = mean_after - mean_before;
            for &id in &col {
                y[id] -= drift;
            }
        }
    }

    // 落地坐标
    let stride = NODE_W + COL_GAP;
    let nodes: Vec<FlowNode> = (0..n)
        .map(|id| FlowNode {
            id: NodeId::from(format!("n{id}").as_str()),
            position: Point::new(height[id] as f64 * stride, y[id] - NODE_H / 2.0),
            size: Size::new(NODE_W, NODE_H),
        })
        .collect();
    let flow_edges: Vec<FlowEdge> = edges
        .iter()
        .map(|&(a, b)| {
            let dashed = match &dag.nodes[b].source {
                PlanSource::Bred { kind, .. } => *kind != BreedKind::Formula,
                _ => false,
            };
            FlowEdge {
                id: format!("n{a}-n{b}"),
                source: NodeId::from(format!("n{a}").as_str()),
                target: NodeId::from(format!("n{b}").as_str()),
                dashed,
                ..Default::default()
            }
        })
        .collect();

    let info: HashMap<String, PlanNode> = dag
        .nodes
        .iter()
        .enumerate()
        .map(|(id, pn)| (format!("n{id}"), pn.clone()))
        .collect();
    let bred_count = count_breedings(root);
    let mut used_owned: Vec<u64> = dag
        .nodes
        .iter()
        .filter_map(|pn| match pn.source {
            PlanSource::Owned { pal_id } => Some(pal_id),
            _ => None,
        })
        .collect();
    used_owned.sort_unstable();
    used_owned.dedup();

    GraphData {
        nodes,
        edges: flow_edges,
        info,
        root_id: format!("n{}", dag.root),
        bred_count,
        used_owned,
    }
}

fn count_breedings(node: &PlanNode) -> usize {
    match &node.source {
        PlanSource::Owned { .. } => 0,
        PlanSource::Bred { p1, p2, .. } => 1 + count_breedings(p1) + count_breedings(p2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::Gender;

    fn leaf(species: &str, id: u64) -> PlanNode {
        PlanNode {
            species: species.into(),
            need_gender: Some(Gender::Male),
            source: PlanSource::Owned { pal_id: id },
        }
    }

    fn bred(species: &str, p1: PlanNode, p2: PlanNode) -> PlanNode {
        PlanNode {
            species: species.into(),
            need_gender: Some(Gender::Female),
            source: PlanSource::Bred {
                kind: BreedKind::Formula,
                p1: Box::new(p1),
                p2: Box::new(p2),
            },
        }
    }

    /// (A×B→C) × D → E
    fn sample_tree() -> PlanNode {
        bred("E", bred("C", leaf("A", 1), leaf("B", 2)), leaf("D", 3))
    }

    /// C 出现两次（结构相同，应折叠）：C×C 不行，用 (A×B→C) × ((A×B→C)×D→F) → G
    fn duplicated_tree() -> PlanNode {
        bred(
            "G",
            bred("C", leaf("A", 1), leaf("B", 2)),
            bred(
                "F",
                bred("C", leaf("A", 1), leaf("B", 2)),
                leaf("D", 3),
            ),
        )
    }

    #[test]
    fn identical_subtrees_are_merged() {
        let g = layout_plan(&duplicated_tree());
        let c_count = g.info.values().filter(|n| n.species == "C").count();
        let a_count = g.info.values().filter(|n| n.species == "A").count();
        assert_eq!(c_count, 1, "相同的 C 子树应折叠为一个节点");
        assert_eq!(a_count, 1, "相同的 A 叶子应折叠");
        // C 应有两个出边（分别指向 G 与 F）
        let c_id = g
            .info
            .iter()
            .find(|(_, n)| n.species == "C")
            .map(|(id, _)| id.clone())
            .unwrap();
        let out = g.edges.iter().filter(|e| e.source.0 == c_id).count();
        assert_eq!(out, 2);
        // 图中 C 被折叠，但实际仍需分别生产两次：C、C、F、G 共 4 次。
        assert_eq!(g.bred_count, 4);
    }

    #[test]
    fn different_gender_requirements_are_not_merged() {
        let mut c2 = bred("C", leaf("A", 1), leaf("B", 2));
        c2.need_gender = Some(Gender::Male);
        let root = bred("G", bred("C", leaf("A", 1), leaf("B", 2)), c2);
        let g = layout_plan(&root);
        let c_count = g.info.values().filter(|n| n.species == "C").count();
        assert_eq!(c_count, 2, "需求性别不同的同种节点不得折叠");
    }

    #[test]
    fn different_owned_pals_are_not_merged() {
        // 同种但不同个体（如被动不同）
        let root = bred("G", leaf("A", 1), leaf("A", 2));
        let g = layout_plan(&root);
        let a_count = g.info.values().filter(|n| n.species == "A").count();
        assert_eq!(a_count, 2);
    }

    #[test]
    fn same_column_nodes_do_not_overlap() {
        for tree in [sample_tree(), duplicated_tree()] {
            let g = layout_plan(&tree);
            let mut columns: HashMap<i64, Vec<f64>> = HashMap::new();
            for node in &g.nodes {
                columns
                    .entry(node.position.x as i64)
                    .or_default()
                    .push(node.position.y);
            }
            for (_, mut ys) in columns {
                ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
                for w in ys.windows(2) {
                    assert!(w[1] - w[0] >= NODE_H - 1e-6, "同列节点不得重叠: {ys:?}");
                }
            }
        }
    }

    #[test]
    fn columns_increase_toward_target() {
        let g = layout_plan(&sample_tree());
        let root = g.nodes.iter().find(|n| n.id.0 == g.root_id).unwrap();
        for n in &g.nodes {
            assert!(n.position.x <= root.position.x);
        }
        assert_eq!(g.bred_count, 2);
        assert_eq!(g.used_owned, vec![1, 2, 3]);
    }
}
