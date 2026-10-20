//! Mirror of the canvas graph, used to resolve names and permissions.
//!
//! Connections are undirected for reachability: an arrow drawn either way
//! connects two nodes. Writing into a terminal additionally requires its
//! "incoming" switch, unless the two are orchestrator and recruit.

use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Terminal,
    Note,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    /// Terminal label, or the note's name (explicit, or its first line).
    pub name: String,
    #[serde(default)]
    pub allow_incoming: bool,
    #[serde(default)]
    pub orchestrator: bool,
    /// Terminal that spawned this one.
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub locked: bool,
}

#[derive(Deserialize, Default)]
pub struct GraphSnapshot {
    pub nodes: Vec<Node>,
    pub edges: Vec<(String, String)>,
}

#[derive(Default, Clone)]
pub struct Graph {
    nodes: Vec<Node>,
    index: HashMap<String, usize>,
    adjacent: HashMap<String, Vec<String>>,
}

impl From<GraphSnapshot> for Graph {
    fn from(snapshot: GraphSnapshot) -> Self {
        let index = snapshot
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.clone(), i))
            .collect::<HashMap<_, _>>();
        let mut adjacent: HashMap<String, Vec<String>> = HashMap::new();
        for (a, b) in snapshot.edges {
            if a == b || !index.contains_key(&a) || !index.contains_key(&b) {
                continue;
            }
            adjacent.entry(a.clone()).or_default().push(b.clone());
            adjacent.entry(b).or_default().push(a);
        }
        for list in adjacent.values_mut() {
            list.sort();
            list.dedup();
        }
        Graph {
            nodes: snapshot.nodes,
            index,
            adjacent,
        }
    }
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

impl Graph {
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.index.get(id).map(|&i| &self.nodes[i])
    }

    fn neighbors(&self, id: &str) -> impl Iterator<Item = &Node> {
        self.adjacent
            .get(id)
            .into_iter()
            .flatten()
            .filter_map(|n| self.node(n))
    }

    pub fn connected(&self, a: &str, b: &str) -> bool {
        self.adjacent
            .get(a)
            .is_some_and(|list| list.iter().any(|n| n == b))
    }

    /// Terminals directly connected to `id`, in canvas order.
    pub fn reachable_terminals(&self, id: &str) -> Vec<&Node> {
        let mut out: Vec<&Node> = self
            .neighbors(id)
            .filter(|n| n.kind == NodeKind::Terminal)
            .collect();
        out.sort_by_key(|n| self.index.get(&n.id).copied());
        out
    }

    /// Whether `from` may write a prompt into `to`.
    pub fn may_message(&self, from: &str, to: &str) -> bool {
        let (Some(source), Some(target)) = (self.node(from), self.node(to)) else {
            return false;
        };
        if !self.connected(from, to) {
            return false;
        }
        target.allow_incoming
            || target.parent.as_deref() == Some(from)
            || source.parent.as_deref() == Some(to)
    }

    /// Notes reachable from terminal `id` with their chain depth: notes wired
    /// to it, notes wired to its recruits when it is an orchestrator, and
    /// every note chained to those through note-to-note connections.
    pub fn reachable_notes(&self, id: &str) -> Vec<(usize, &Node)> {
        let mut roots: Vec<&Node> = self
            .neighbors(id)
            .filter(|n| n.kind == NodeKind::Note)
            .collect();
        if self.node(id).is_some_and(|n| n.orchestrator) {
            for recruit in self.recruits(id) {
                roots.extend(self.neighbors(&recruit.id).filter(|n| n.kind == NodeKind::Note));
            }
        }
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for root in roots {
            if !seen.insert(root.id.clone()) {
                continue;
            }
            let mut queue = VecDeque::from([(0usize, root)]);
            while let Some((depth, note)) = queue.pop_front() {
                out.push((depth, note));
                for next in self.neighbors(&note.id) {
                    if next.kind == NodeKind::Note && seen.insert(next.id.clone()) {
                        queue.push_back((depth + 1, next));
                    }
                }
            }
        }
        out
    }

    /// Terminals spawned by `id`.
    pub fn recruits(&self, id: &str) -> Vec<&Node> {
        self.nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Terminal && n.parent.as_deref() == Some(id))
            .collect()
    }

    pub fn find_terminal(&self, from: &str, name: &str) -> Option<&Node> {
        self.reachable_terminals(from)
            .into_iter()
            .find(|n| same_name(&n.name, name))
    }

    pub fn find_note(&self, from: &str, name: &str) -> Option<&Node> {
        self.reachable_notes(from)
            .into_iter()
            .map(|(_, n)| n)
            .find(|n| same_name(&n.name, name))
    }

    /// Orchestrator terminals wired to a note, directly or through a chain.
    pub fn orchestrators_of_note(&self, note: &str) -> Vec<&Node> {
        self.nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Terminal && n.orchestrator)
            .filter(|n| self.reachable_notes(&n.id).iter().any(|(_, x)| x.id == note))
            .collect()
    }

    pub fn terminal_names(&self) -> Vec<&str> {
        self.nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Terminal)
            .map(|n| n.name.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, kind: NodeKind, name: &str) -> Node {
        Node {
            id: id.into(),
            kind,
            name: name.into(),
            allow_incoming: false,
            orchestrator: false,
            parent: None,
            locked: false,
        }
    }

    #[test]
    fn notes_chain_and_permissions() {
        let mut boss = node("a", NodeKind::Terminal, "Boss");
        boss.orchestrator = true;
        let mut worker = node("b", NodeKind::Terminal, "Worker");
        worker.parent = Some("a".into());
        let stranger = node("c", NodeKind::Terminal, "Stranger");
        let graph = Graph::from(GraphSnapshot {
            nodes: vec![
                boss,
                worker,
                stranger,
                node("n1", NodeKind::Note, "Brief"),
                node("n2", NodeKind::Note, "Plan"),
                node("n3", NodeKind::Note, "Scratch"),
            ],
            edges: vec![
                ("a".into(), "b".into()),
                ("c".into(), "a".into()),
                ("n1".into(), "a".into()),
                ("n1".into(), "n2".into()),
                ("b".into(), "n3".into()),
            ],
        });
        let notes: Vec<_> = graph
            .reachable_notes("a")
            .iter()
            .map(|(d, n)| (*d, n.name.clone()))
            .collect();
        assert_eq!(
            notes,
            vec![(0, "Brief".into()), (1, "Plan".into()), (0, "Scratch".into())]
        );
        assert!(graph.may_message("a", "b"));
        assert!(graph.may_message("b", "a"));
        assert!(!graph.may_message("a", "c"));
        assert!(graph.find_terminal("a", "worker").is_some());
        assert!(graph.find_note("b", "brief").is_none());
    }
}
