//! Deterministic benchmark state machine and loop control for `std.testing.Benchmark`.

use std::cell::RefCell;

use super::types::MAX_BENCH_ITERATIONS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BenchmarkPhase {
    Created,
    Warmup {
        started_ns: u64,
        iterations: u64,
    },
    Measuring {
        started_ns: u64,
        iterations: u64,
        batch_iterations: u64,
    },
    Finished,
}

/// Deterministic benchmark state machine. Time is injected as monotonic
/// nanoseconds so rollback, resolution and overflow paths are unit-testable.
#[derive(Debug)]
pub struct BenchmarkEngine {
    pub(crate) config: arandu_testing_contract::BenchmarkConfigV1,
    pub(crate) phase: BenchmarkPhase,
    pub(crate) samples: Vec<arandu_testing_contract::BenchmarkSampleV1>,
    pub(crate) failure: Option<String>,
}

impl BenchmarkEngine {
    #[must_use]
    pub fn new(config: arandu_testing_contract::BenchmarkConfigV1) -> Self {
        let failure = if config.warmup_ns == 0 {
            Some("benchmark warmup must be greater than zero".to_string())
        } else if config.measurement_ns == 0 {
            Some("benchmark measurement time must be greater than zero".to_string())
        } else if config.samples == 0 || config.samples > 10_000 {
            Some("benchmark sample count must be between 1 and 10000".to_string())
        } else {
            None
        };
        let phase = if failure.is_some() {
            BenchmarkPhase::Finished
        } else {
            BenchmarkPhase::Created
        };
        Self {
            config,
            phase,
            samples: Vec::new(),
            failure,
        }
    }

    /// Complete the preceding iteration and decide whether another should run.
    pub fn advance(&mut self, now_ns: u64) -> bool {
        match self.phase {
            BenchmarkPhase::Created => {
                self.phase = BenchmarkPhase::Warmup {
                    started_ns: now_ns,
                    iterations: 0,
                };
                true
            }
            BenchmarkPhase::Warmup {
                started_ns,
                iterations,
            } => {
                let Some(elapsed) = now_ns.checked_sub(started_ns) else {
                    self.fail("monotonic benchmark clock moved backwards");
                    return false;
                };
                let iterations = iterations.saturating_add(1).min(MAX_BENCH_ITERATIONS);
                if elapsed < self.config.warmup_ns || elapsed == 0 {
                    if iterations >= MAX_BENCH_ITERATIONS {
                        self.fail("benchmark calibration exceeded the iteration limit");
                        return false;
                    }
                    self.phase = BenchmarkPhase::Warmup {
                        started_ns,
                        iterations,
                    };
                    return true;
                }
                let per_iteration = elapsed.div_ceil(iterations).max(1);
                let per_sample_target = self
                    .config
                    .measurement_ns
                    .div_ceil(u64::from(self.config.samples.max(1)));
                let batch_iterations = per_sample_target
                    .div_ceil(per_iteration)
                    .clamp(1, MAX_BENCH_ITERATIONS);
                self.phase = BenchmarkPhase::Measuring {
                    started_ns: now_ns,
                    iterations: 0,
                    batch_iterations,
                };
                true
            }
            BenchmarkPhase::Measuring {
                started_ns,
                iterations,
                batch_iterations,
            } => {
                let iterations = iterations.saturating_add(1);
                if iterations < batch_iterations {
                    self.phase = BenchmarkPhase::Measuring {
                        started_ns,
                        iterations,
                        batch_iterations,
                    };
                    return true;
                }
                let Some(elapsed_ns) = now_ns.checked_sub(started_ns) else {
                    self.fail("monotonic benchmark clock moved backwards");
                    return false;
                };
                if elapsed_ns == 0 {
                    let Some(larger_batch) = batch_iterations
                        .checked_mul(2)
                        .filter(|value| *value <= MAX_BENCH_ITERATIONS)
                    else {
                        self.fail("benchmark remained below the monotonic clock resolution");
                        return false;
                    };
                    self.phase = BenchmarkPhase::Measuring {
                        started_ns: now_ns,
                        iterations: 0,
                        batch_iterations: larger_batch,
                    };
                    return true;
                }
                self.samples
                    .push(arandu_testing_contract::BenchmarkSampleV1 {
                        iterations,
                        elapsed_ns,
                    });
                if self.samples.len() >= self.config.samples as usize {
                    self.phase = BenchmarkPhase::Finished;
                    false
                } else {
                    self.phase = BenchmarkPhase::Measuring {
                        started_ns: now_ns,
                        iterations: 0,
                        batch_iterations,
                    };
                    true
                }
            }
            BenchmarkPhase::Finished => false,
        }
    }

    fn fail(&mut self, message: &str) {
        self.failure = Some(message.to_string());
        self.phase = BenchmarkPhase::Finished;
    }
}

struct ActiveBenchmark {
    id: String,
    sequence: u64,
    origin: std::time::Instant,
    engine: BenchmarkEngine,
}

thread_local! {
    static ACTIVE_BENCHMARK: RefCell<Option<ActiveBenchmark>> = const { RefCell::new(None) };
}

pub fn init_benchmark_context(
    id: &str,
    sequence: u64,
    config: arandu_testing_contract::BenchmarkConfigV1,
) {
    ACTIVE_BENCHMARK.with(|cell| {
        *cell.borrow_mut() = Some(ActiveBenchmark {
            id: id.to_string(),
            sequence,
            origin: std::time::Instant::now(),
            engine: BenchmarkEngine::new(config),
        });
    });
}

#[must_use]
pub fn finish_benchmark_context() -> Option<arandu_testing_contract::BenchmarkEventV1> {
    ACTIVE_BENCHMARK.with(|cell| {
        cell.borrow_mut()
            .take()
            .map(|active| arandu_testing_contract::BenchmarkEventV1 {
                sequence: active.sequence,
                id: active.id,
                config: active.engine.config,
                samples: active.engine.samples,
                stdout: arandu_testing_contract::CapturedOutput::default(),
                stderr: arandu_testing_contract::CapturedOutput::default(),
                failure: active.engine.failure,
            })
    })
}

/// Benchmark loop control called from `std.testing.Benchmark.loop`.
#[unsafe(no_mangle)]
pub extern "C" fn ar_bench_loop(_handle: i64) -> i64 {
    ACTIVE_BENCHMARK.with(|cell| {
        let mut active = cell.borrow_mut();
        let Some(active) = active.as_mut() else {
            return 0;
        };
        let elapsed = active.origin.elapsed().as_nanos();
        let now_ns = u64::try_from(elapsed).unwrap_or(u64::MAX);
        i64::from(active.engine.advance(now_ns))
    })
}
