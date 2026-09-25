//! Per-JIT recorder for opt-in basic-block coverage, shared safely by workers.

use arandu_base::Span;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

const MAX_BLOCK_COVERAGE_HITS: usize = 65_536;
static NEXT_SESSION_ID: AtomicI64 = AtomicI64::new(1);
static SESSIONS: OnceLock<Mutex<HashMap<i64, Weak<BlockCoverageSession>>>> = OnceLock::new();

/// One basic block entered by a JIT program compiled with coverage enabled.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BlockCoverageHit {
    /// Dense index of the function in the compiled `AmirProgram`.
    pub function_index: u32,
    /// Dense index of the block in that function's AMIR CFG.
    pub block_index: u32,
    /// Source block associated with this AMIR block, if available.
    pub source_span: Option<Span>,
}

/// Bounded execution trace returned by an instrumented JIT run.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BlockCoverage {
    /// Ordered basic-block entries collected before the cap was reached.
    pub hits: Vec<BlockCoverageHit>,
    /// Whether more block entries occurred after the trace cap.
    pub truncated: bool,
    /// Source mapping for all AMIR blocks in the instrumented program.
    pub source_blocks: Vec<BlockSourceMapping>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockSourceMapping {
    pub function_index: u32,
    pub block_index: u32,
    pub span: Span,
}

#[derive(Default)]
struct Recorder {
    hits: Vec<BlockCoverageHit>,
    truncated: bool,
}

/// Coverage state owned by one instrumented compiled module.
pub(crate) struct BlockCoverageSession {
    id: i64,
    recorder: Mutex<Recorder>,
    source_spans: HashMap<(u32, u32), Span>,
}

impl BlockCoverageSession {
    pub(crate) fn new(program: &arandu_semantics::amir::AmirProgram) -> Option<Arc<Self>> {
        let id = NEXT_SESSION_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .ok()?;
        let session = Arc::new(Self {
            id,
            recorder: Mutex::new(Recorder::default()),
            source_spans: source_spans(program),
        });
        sessions()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, Arc::downgrade(&session));
        Some(session)
    }

    pub(crate) fn id(&self) -> i64 {
        self.id
    }

    pub(crate) fn take(&self) -> BlockCoverage {
        let recorder = std::mem::take(
            &mut *self
                .recorder
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let mut source_blocks = self
            .source_spans
            .iter()
            .map(
                |(&(function_index, block_index), &span)| BlockSourceMapping {
                    function_index,
                    block_index,
                    span,
                },
            )
            .collect::<Vec<_>>();
        source_blocks.sort_unstable_by_key(|mapping| (mapping.function_index, mapping.block_index));
        BlockCoverage {
            hits: recorder.hits,
            truncated: recorder.truncated,
            source_blocks,
        }
    }
}

fn source_spans(program: &arandu_semantics::amir::AmirProgram) -> HashMap<(u32, u32), Span> {
    let function_indices = program
        .funcs
        .iter()
        .enumerate()
        .filter_map(|(index, function)| {
            u32::try_from(index)
                .ok()
                .map(|index| (function.symbol, index, function.blocks.len()))
        })
        .map(|(symbol, index, len)| (symbol, (index, len)))
        .collect::<HashMap<_, _>>();
    program
        .debug_blocks
        .iter()
        .filter_map(|debug_block| {
            let (function_index, block_count) = *function_indices.get(&debug_block.function)?;
            let block_index = u32::try_from(debug_block.block.as_usize()).ok()?;
            (debug_block.block.as_usize() < block_count)
                .then_some(((function_index, block_index), debug_block.span))
        })
        .collect()
}

impl BlockCoverageSession {
    fn source_span(&self, function_index: u32, block_index: u32) -> Option<Span> {
        self.source_spans
            .get(&(function_index, block_index))
            .copied()
    }
}

impl Drop for BlockCoverageSession {
    fn drop(&mut self) {
        sessions()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
    }
}

fn sessions() -> &'static Mutex<HashMap<i64, Weak<BlockCoverageSession>>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cranelift import called at the start of each instrumented AMIR block.
pub(crate) extern "C" fn record_block_hit(session_id: i64, function_index: i64, block_index: i64) {
    let (Ok(function_index), Ok(block_index)) =
        (u32::try_from(function_index), u32::try_from(block_index))
    else {
        return;
    };
    let session = sessions()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&session_id)
        .and_then(Weak::upgrade);
    let Some(session) = session else {
        return;
    };
    let hit = BlockCoverageHit {
        function_index,
        block_index,
        source_span: session.source_span(function_index, block_index),
    };
    let mut recorder = session
        .recorder
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if recorder.hits.len() < MAX_BLOCK_COVERAGE_HITS {
        recorder.hits.push(hit);
    } else {
        recorder.truncated = true;
    }
}

#[cfg(test)]
fn empty_program() -> arandu_semantics::amir::AmirProgram {
    arandu_semantics::amir::AmirProgram {
        funcs: Vec::new(),
        literal_pool: arandu_semantics::literal_pool::AmirLiteralPool::default(),
        extern_funcs: Default::default(),
        debug_bindings: Vec::new(),
        debug_blocks: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_caps_memory_and_marks_truncated_traces() {
        let session = BlockCoverageSession::new(&empty_program()).unwrap();
        for _ in 0..=MAX_BLOCK_COVERAGE_HITS {
            record_block_hit(session.id(), 0, 0);
        }

        let coverage = session.take();
        assert_eq!(coverage.hits.len(), MAX_BLOCK_COVERAGE_HITS);
        assert!(coverage.truncated);
    }

    #[test]
    fn recorder_collects_worker_hits_and_isolates_modules() {
        let empty = empty_program();
        let first = BlockCoverageSession::new(&empty).unwrap();
        let second = BlockCoverageSession::new(&empty).unwrap();
        let first_id = first.id();
        let mut workers = Vec::new();
        for worker in 0..4_i64 {
            workers.push(std::thread::spawn(move || {
                for block in 0..8_i64 {
                    record_block_hit(first_id, worker, block);
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        record_block_hit(second.id(), 99, 100);

        let first_coverage = first.take();
        let second_coverage = second.take();
        assert_eq!(first_coverage.hits.len(), 32);
        assert!(!first_coverage.truncated);
        assert_eq!(second_coverage.hits.len(), 1);
        assert_eq!(second_coverage.hits[0].function_index, 99);
        assert_eq!(second_coverage.hits[0].block_index, 100);
    }
}
