use arandu_middle::newtype_index;
pub use arandu_middle::symbol_mangling::InstantiationKey;
use arandu_middle::symbol_mangling::mangle_symbol;
use arandu_middle::symbol_table::{SymbolId, SymbolTable};
use arandu_middle::types::TypeInterner;
use rustc_hash::FxHashMap;

newtype_index!(InstantiationNodeId);

#[derive(Debug, Clone)]
pub struct InstantiationNode<'bump> {
    pub id: InstantiationNodeId,
    pub key: InstantiationKey<'bump>,
    pub mangled_name: &'bump str,
    pub callees: bumpalo::collections::Vec<'bump, InstantiationNodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonoError {
    RecursionLimitExceeded { symbol: SymbolId, limit: usize },
}

#[derive(Debug)]
pub struct InstantiationGraph<'bump> {
    nodes: bumpalo::collections::Vec<'bump, InstantiationNode<'bump>>,
    index: FxHashMap<InstantiationKey<'bump>, InstantiationNodeId>,
    instantiation_counts: FxHashMap<SymbolId, usize>,
    recursion_limit: usize,
}

impl<'bump> InstantiationGraph<'bump> {
    #[must_use]
    pub fn new(bump: &'bump bumpalo::Bump) -> Self {
        Self {
            nodes: bumpalo::collections::Vec::new_in(bump),
            index: FxHashMap::default(),
            instantiation_counts: FxHashMap::default(),
            recursion_limit: 64,
        }
    }

    #[must_use]
    pub fn with_recursion_limit(bump: &'bump bumpalo::Bump, limit: usize) -> Self {
        Self {
            nodes: bumpalo::collections::Vec::new_in(bump),
            index: FxHashMap::default(),
            instantiation_counts: FxHashMap::default(),
            recursion_limit: limit,
        }
    }

    pub fn get_or_insert(
        &mut self,
        key: &InstantiationKey<'bump>,
        bump: &'bump bumpalo::Bump,
        interner: &TypeInterner,
        symbols: &SymbolTable,
    ) -> Result<InstantiationNodeId, MonoError> {
        if let Some(&id) = self.index.get(key) {
            return Ok(id);
        }

        let same_symbol_count = self
            .instantiation_counts
            .get(&key.symbol)
            .copied()
            .unwrap_or(0);
        if same_symbol_count >= self.recursion_limit {
            return Err(MonoError::RecursionLimitExceeded {
                symbol: key.symbol,
                limit: self.recursion_limit,
            });
        }

        let mangled = mangle_symbol(key, interner, symbols);
        let mangled_bump = bump.alloc_str(&mangled);
        let id = InstantiationNodeId::from_usize(self.nodes.len());
        self.nodes.push(InstantiationNode {
            id,
            key: *key,
            mangled_name: mangled_bump,
            callees: bumpalo::collections::Vec::new_in(bump),
        });
        self.index.insert(*key, id);
        *self.instantiation_counts.entry(key.symbol).or_insert(0) += 1;
        Ok(id)
    }

    pub fn add_edge(&mut self, caller: InstantiationNodeId, callee: InstantiationNodeId) {
        self.nodes[caller.as_usize()].callees.push(callee);
    }

    #[must_use]
    pub fn get_node(&self, id: InstantiationNodeId) -> &InstantiationNode<'bump> {
        &self.nodes[id.as_usize()]
    }

    #[must_use]
    pub fn lookup(&self, key: &InstantiationKey<'bump>) -> Option<InstantiationNodeId> {
        self.index.get(key).copied()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &InstantiationNode<'bump>> {
        self.nodes.iter()
    }

    #[must_use]
    pub fn find_cycle(&self) -> Option<Vec<InstantiationNodeId>> {
        let n = self.nodes.len();
        let mut visited = vec![false; n];
        let mut on_stack = vec![false; n];
        let mut path = Vec::new();

        for i in 0..n {
            if !visited[i]
                && let Some(cycle) = self.dfs_cycle(
                    InstantiationNodeId::from_usize(i),
                    &mut visited,
                    &mut on_stack,
                    &mut path,
                )
            {
                return Some(cycle);
            }
        }
        None
    }

    fn dfs_cycle(
        &self,
        node: InstantiationNodeId,
        visited: &mut Vec<bool>,
        on_stack: &mut Vec<bool>,
        path: &mut Vec<InstantiationNodeId>,
    ) -> Option<Vec<InstantiationNodeId>> {
        let idx = node.as_usize();
        visited[idx] = true;
        on_stack[idx] = true;
        path.push(node);

        for &callee in &self.nodes[idx].callees {
            let ci = callee.as_usize();
            if !visited[ci] {
                if let Some(cycle) = self.dfs_cycle(callee, visited, on_stack, path) {
                    return Some(cycle);
                }
            } else if on_stack[ci] {
                let start = path.iter().position(|&n| n == callee).unwrap_or(0);
                return Some(path[start..].to_vec());
            }
        }

        path.pop();
        on_stack[idx] = false;
        None
    }
}
