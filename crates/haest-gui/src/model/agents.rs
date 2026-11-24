//! The agents of one conversation as a tree: main at the root, each
//! subagent under the agent that started it.
//!
//! `subagent_started` names the parent by its address (`main`, or another
//! subagent's name); a frame's path carries the subagent's id. A name is
//! unique while it runs, so a parent is found among the running ones first.

/// The root agent's id: the conversation's own agent.
pub const MAIN: &str = "main";

/// Where an agent is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// It runs.
    Running,
    /// It finished its work.
    Finished,
    /// It stopped before finishing: stopped, cut off, or failed.
    StoppedEarly,
}

/// One agent.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentNode {
    /// Its id: the path's segment (`main` for the root).
    pub id: String,
    /// Its address, as the agent tools name it.
    pub name: String,
    /// Its parent's id.
    pub parent: Option<String>,
    /// What its parent asked it to do.
    pub description: String,
    /// Where it is.
    pub state: AgentState,
    /// A model call of its streams now.
    pub streaming: bool,
    /// When it started, ms since the epoch.
    pub started_ts: u64,
    /// When it stopped, while it is not running.
    pub ended_ts: Option<u64>,
    /// Its turns so far (a subagent may be sent more work).
    pub runs: usize,
}

/// The tree.
#[derive(Debug, Clone)]
pub struct AgentTree {
    nodes: Vec<AgentNode>,
}

impl Default for AgentTree {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentTree {
    /// A tree of main alone.
    pub fn new() -> Self {
        Self {
            nodes: vec![AgentNode {
                id: MAIN.into(),
                name: MAIN.into(),
                parent: None,
                description: String::new(),
                state: AgentState::Running,
                streaming: false,
                started_ts: 0,
                ended_ts: None,
                runs: 1,
            }],
        }
    }

    /// The agent `id`, when the tree has it.
    pub fn get(&self, id: &str) -> Option<&AgentNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut AgentNode> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// The id of the agent `name` names: running ones first, then the
    /// latest; `main` when none.
    pub fn id_of(&self, name: &str) -> String {
        if name == MAIN {
            return MAIN.into();
        }
        let named = |n: &&AgentNode| n.name == name;
        self.nodes
            .iter()
            .rev()
            .filter(named)
            .find(|n| n.state == AgentState::Running)
            .or_else(|| self.nodes.iter().rev().find(named))
            .map_or_else(|| MAIN.into(), |n| n.id.clone())
    }

    /// The subagent `id` started (again).
    pub fn started(&mut self, id: &str, name: &str, parent: &str, description: &str, ts: u64) {
        let parent = self.id_of(parent);
        // An agent cannot be its own parent (a name reused by a new run).
        let parent = if parent == id {
            MAIN.to_string()
        } else {
            parent
        };
        match self.get_mut(id) {
            Some(node) => {
                node.state = AgentState::Running;
                node.description = description.into();
                node.ended_ts = None;
                node.runs += 1;
            }
            None => self.nodes.push(AgentNode {
                id: id.into(),
                name: name.into(),
                parent: Some(parent),
                description: description.into(),
                state: AgentState::Running,
                streaming: false,
                started_ts: ts,
                ended_ts: None,
                runs: 1,
            }),
        }
    }

    /// The subagent `id` stopped.
    pub fn stopped(&mut self, id: &str, ok: bool, ts: u64) {
        if let Some(node) = self.get_mut(id) {
            node.state = if ok {
                AgentState::Finished
            } else {
                AgentState::StoppedEarly
            };
            node.streaming = false;
            node.ended_ts = Some(ts);
        }
    }

    /// Whether `id` streams a model call now.
    pub fn streaming(&mut self, id: &str, on: bool) {
        if let Some(node) = self.get_mut(id) {
            node.streaming = on;
        }
    }

    /// The children of `id`, in the order they started.
    pub fn children<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a AgentNode> + 'a {
        self.nodes
            .iter()
            .filter(move |n| n.parent.as_deref() == Some(id))
    }

    /// Every node depth-first, with its depth: the order the tree view lists.
    pub fn flatten(&self) -> Vec<(usize, &AgentNode)> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack = vec![(0usize, &self.nodes[0])];
        while let Some((depth, node)) = stack.pop() {
            out.push((depth, node));
            let kids: Vec<_> = self.children(&node.id).collect();
            for kid in kids.into_iter().rev() {
                stack.push((depth + 1, kid));
            }
        }
        out
    }

    /// How many subagents there are (main not counted).
    pub fn subagents(&self) -> usize {
        self.nodes.len() - 1
    }

    /// How many run now.
    pub fn running(&self) -> usize {
        self.nodes[1..]
            .iter()
            .filter(|n| n.state == AgentState::Running)
            .count()
    }
}
