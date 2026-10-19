//! Runtime groups and dependency closure collected from live AMIR.
use crate::emitter::CEmitter;
use arandu_middle::amir::{AmirConstant, AmirOperand, AmirRvalue, AmirStmt};
use arandu_middle::literal_pool::AmirLiteralEntry;
use arandu_middle::ops::{BinaryOp, UnaryOp};
use arandu_middle::types::{ArType, Primitive};

#[derive(Default)]
pub(super) struct RuntimeFeatures {
    pub arena: bool,
    pub buffers: bool,
    pub coroutine: bool,
    pub tasks: bool,
    pub native_threads: bool,
    pub fs: bool,
    pub env: bool,
    pub path: bool,
    pub string_eq: bool,
    pub concat: bool,
    /// Signed, unsigned, float, bool, char.
    pub formats: [bool; 5],
    pub float_bits: [bool; 2],
    pub black_box: [bool; 4],
}

impl CEmitter<'_> {
    pub(super) fn runtime_features(&self) -> RuntimeFeatures {
        let mut features = RuntimeFeatures::default();
        for function in &self.program.funcs {
            if !self.live_functions.contains(&function.symbol) {
                continue;
            }
            let mut host = |op: &AmirOperand| {
                if let AmirOperand::FunctionRef(symbol) = op {
                    features.host(self.symbols.host_func_name(self.symbols.get(*symbol)));
                }
                if let AmirOperand::Constant(AmirConstant::Pool(id)) = op
                    && let AmirLiteralEntry::FloatBits(value) = self.program.literal_pool.get(*id)
                {
                    features.float_bits[usize::from(value.ty().bit_width() != 32)] = true;
                }
            };
            for stmt in function.stmts.payloads.iter() {
                arandu_middle::amir::visit::for_each_stmt_operand(stmt, &mut host);
            }
            for block in &function.blocks {
                arandu_middle::amir::visit::for_each_terminator_operand(
                    &block.terminator,
                    &mut host,
                );
            }
            for stmt in function.stmts.payloads.iter() {
                if let AmirStmt::Assign { rhs, .. } = stmt {
                    match rhs {
                        AmirRvalue::GenInsert { .. }
                        | AmirRvalue::GenGet { .. }
                        | AmirRvalue::GenSet { .. }
                        | AmirRvalue::GenUpsert { .. }
                        | AmirRvalue::GenRemove { .. } => features.arena = true,
                        AmirRvalue::CoroutineReady { .. } => features.coroutine = true,
                        AmirRvalue::Unary {
                            op: UnaryOp::Await, ..
                        } => features.coroutine = true,
                        AmirRvalue::BlackBox { value_ty, .. } => {
                            let index = match self.interner.resolve(*value_ty) {
                                ArType::FloatLiteral => Some(1),
                                ArType::Primitive(Primitive::F32) => Some(3),
                                ArType::Primitive(p) if p.is_float() => Some(1),
                                ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_) => Some(2),
                                ArType::Primitive(Primitive::Str) => None,
                                ArType::Primitive(_) | ArType::IntLiteral => Some(0),
                                _ => None,
                            };
                            if let Some(index) = index {
                                features.black_box[index] = true;
                            }
                        }
                        AmirRvalue::StringInterp { .. } => features.concat = true,
                        AmirRvalue::Binary { op, left, right } => {
                            features.coroutine |=
                                matches!(op, BinaryOp::RangeExclusive | BinaryOp::RangeInclusive);
                            features.string_eq |= [left, right].into_iter().any(|operand| {
                                self.operand_ty(function, operand)
                                    .str_reference_depth(self.interner)
                                    .is_some()
                            });
                        }
                        AmirRvalue::ToStr { src_ty, .. } => {
                            let index = match self.interner.resolve(*src_ty) {
                                ArType::Primitive(Primitive::Bool) => Some(3),
                                ArType::Primitive(Primitive::Char) => Some(4),
                                ArType::FloatLiteral => Some(2),
                                ArType::IntLiteral => Some(0),
                                ArType::Primitive(p) if p.is_float() => Some(2),
                                ArType::Primitive(p) if p.is_integer() => {
                                    Some(usize::from(!p.is_signed()))
                                }
                                _ => None,
                            };
                            if let Some(index) = index {
                                features.formats[index] = true;
                            }
                        }
                        AmirRvalue::Use(_)
                        | AmirRvalue::Unary { .. }
                        | AmirRvalue::FieldAccess { .. }
                        | AmirRvalue::StructLiteral { .. }
                        | AmirRvalue::IndexAccess { .. }
                        | AmirRvalue::Array { .. }
                        | AmirRvalue::Tuple { .. }
                        | AmirRvalue::Discriminant { .. }
                        | AmirRvalue::EnumPayload { .. }
                        | AmirRvalue::EnumConstruct { .. }
                        | AmirRvalue::Len(_)
                        | AmirRvalue::SliceData(_)
                        | AmirRvalue::SliceView { .. }
                        | AmirRvalue::SliceSubslice { .. }
                        | AmirRvalue::StrBytes { .. }
                        | AmirRvalue::StrView { .. }
                        | AmirRvalue::Alloc(_)
                        | AmirRvalue::Load(_)
                        | AmirRvalue::Borrow(_)
                        | AmirRvalue::BorrowMut(_)
                        | AmirRvalue::VectorCompare { .. }
                        | AmirRvalue::VectorSelect { .. }
                        | AmirRvalue::VectorOp { .. }
                        | AmirRvalue::VectorSplat { .. }
                        | AmirRvalue::VectorShuffle { .. }
                        | AmirRvalue::VectorExtractLane { .. }
                        | AmirRvalue::VectorInsertLane { .. }
                        | AmirRvalue::RelativeBorrow { .. } => {}
                    }
                }
            }
        }
        features.coroutine |= features.tasks;
        // The path group includes owned joining, whose buffer comes from Vec.
        features.buffers |= features.path || features.fs;
        features
    }
}

impl RuntimeFeatures {
    fn host(&mut self, name: &str) {
        let name = name.rsplit('.').next().unwrap_or(name);
        self.native_threads |= name.starts_with("ar_rt_native_thread_");
        self.buffers |= name.starts_with("ar_vec_")
            || name.starts_with("ar_rt_native_thread_")
            || name == "ar_string_push_str"
            || matches!(
                name,
                "ar_rt_copy_value" | "ar_rt_alloc_aligned" | "ar_rt_free_aligned"
            );
        self.string_eq |= name == "ar_str_eq";
        self.path |=
            name.starts_with("ar_path_") || (name.starts_with("ar_str_") && name != "ar_str_eq");
        self.fs |= name.starts_with("ar_fs_") || name == "exists" || name.starts_with("ar_net_");
        self.env |= name.starts_with("ar_env_")
            || matches!(name, "ar_time_monotonic_ns" | "ar_process_exit");
        self.arena |= name.starts_with("ar_gen_");
        self.coroutine |= name.starts_with("ar_co_");
        self.tasks |= matches!(
            name,
            "ar_rt_block_on_i64"
                | "ar_rt_spawn_i64"
                | "ar_rt_join_i64"
                | "ar_rt_cancel_i64"
                | "ar_rt_parallel_fold_run"
                | "ar_rt_thread_sleep_ms"
                | "ar_rt_thread_yield"
                | "ar_rt_thread_park"
                | "ar_rt_thread_park_timeout"
                | "ar_rt_thread_unpark"
                | "ar_rt_thread_current_id"
                | "ar_rt_condvar_wait"
                | "ar_rt_condvar_wait_timeout"
                | "ar_rt_condvar_notify_one"
                | "ar_rt_condvar_notify_all"
                | "ar_rt_get_entropy"
        ) || name.starts_with("ar_rt_native_thread_")
            || name.starts_with("ar_rt_rwlock_");
    }
}
