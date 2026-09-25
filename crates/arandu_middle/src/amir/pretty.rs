use super::local::TempId;
use super::program::{AmirFunc, AmirProgram};
use super::stmt::{AmirStmt, AmirTerminator};
use super::value::{AmirConstant, AmirOperand, AmirPlace, AmirProjection, AmirRvalue};
use crate::SymbolTable;
use crate::hir::ReceiverKind;
use crate::literal_pool::{AmirLiteralEntry, AmirLiteralPool};
use crate::ops::{BinaryOp, UnaryOp};

impl AmirProgram {
    #[must_use]
    pub fn pretty_print(
        &self,
        symbols: &SymbolTable,
        interner: &crate::types::TypeInterner,
    ) -> String {
        let mut out = String::new();
        for (i, func) in self.funcs.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            func.pretty_print_to(&mut out, symbols, &self.literal_pool, interner);
        }
        out
    }
}

fn receiver_kind_prefix(kind: ReceiverKind) -> &'static str {
    match kind {
        ReceiverKind::Shared => "shared ",
        ReceiverKind::Mut => "mut ",
        ReceiverKind::Own => "own ",
    }
}

impl AmirFunc {
    fn pretty_print_to(
        &self,
        out: &mut String,
        symbols: &SymbolTable,
        pool: &AmirLiteralPool,
        interner: &crate::types::TypeInterner,
    ) {
        let param_strs: Vec<String> = self
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let ty = self
                    .temps
                    .get(p.as_usize())
                    .map_or_else(|| crate::types::ArType::Void, |t| interner.resolve(t.ty));
                let prefix = self
                    .receiver
                    .as_ref()
                    .filter(|recv| recv.temp == *p)
                    .map_or("", |recv| receiver_kind_prefix(recv.kind));
                // Corresponding local is at index i
                let name_str = self
                    .locals
                    .get(i)
                    .and_then(|l| l.symbol)
                    .map_or("param", |s| symbols.get(s).name.as_str());
                format!("{prefix}{name_str}: {}", ty.display(symbols, interner))
            })
            .collect();
        out.push_str(&format!(
            "Func {}({}) -> {}\n",
            symbols.get(self.symbol).name,
            param_strs.join(", "),
            interner
                .resolve(self.return_type)
                .display(symbols, interner)
        ));

        out.push_str("  locals:\n");
        // Print SSA temporary registers
        for temp in &self.temps {
            let comment = if temp.id == TempId(0) {
                " // return".to_string()
            } else {
                String::new()
            };
            out.push_str(&format!(
                "    _{}: {}{}\n",
                temp.id.0,
                interner.resolve(temp.ty).display(symbols, interner),
                comment
            ));
        }
        // Print stack local variables
        for local in &self.locals {
            let comment = if let Some(sym) = local.symbol {
                format!(" // {}", symbols.get(sym).name)
            } else {
                String::new()
            };
            out.push_str(&format!(
                "    s{}: {}{}\n",
                local.id.0,
                interner.resolve(local.ty).display(symbols, interner),
                comment
            ));
        }

        out.push('\n');

        // Basic blocks
        for block in &self.blocks {
            let block_params = self.block_params(block.params);
            let mut param_str = String::new();
            if !block_params.is_empty() {
                let p_strs: Vec<String> = block_params
                    .iter()
                    .map(|p| {
                        let mut s = format!(
                            "_{}: {}",
                            p.id.0,
                            interner.resolve(p.ty).display(symbols, interner)
                        );
                        if let Some(ref from) = p.from {
                            s.push_str(&format!(" /* from: \"{from}\" */"));
                        }
                        if p.moved {
                            s.push_str(" @moved");
                        }
                        s
                    })
                    .collect();
                param_str = format!("({})", p_strs.join(", "));
            }
            out.push_str(&format!("  bb{}{}:\n", block.id.0, param_str));
            for stmt in self.block_stmts(block.id) {
                out.push_str("    ");
                stmt.pretty_print_to(out, symbols, pool);
                out.push('\n');
            }
            out.push_str("    ");
            block.terminator.pretty_print_to(out, symbols, pool);
            out.push('\n');
        }
    }
}

impl AmirPlace {
    pub fn pretty_print_to(&self, out: &mut String, symbols: &SymbolTable, pool: &AmirLiteralPool) {
        // Build path in a scratch buffer so Deref wrap never steals a caller prefix
        // (e.g. `&` from Borrow pretty-print).
        let mut path = format!("s{}", self.local.0);
        for proj in &self.projections {
            match proj {
                AmirProjection::Deref => {
                    // Wrap so `Deref` then `Field` prints as `(*sN).f`, not `*sN.f`.
                    path = format!("(*{path})");
                }
                AmirProjection::Field(symbol) => {
                    path.push_str(&format!(".{}", symbols.get(*symbol).name));
                }
                AmirProjection::Index(op) => {
                    path.push_str(&format!("[{}]", op.to_pretty_string(symbols, pool)));
                }
            }
        }
        out.push_str(&path);
    }
}

impl AmirStmt {
    pub fn pretty_print_to(&self, out: &mut String, symbols: &SymbolTable, pool: &AmirLiteralPool) {
        match self {
            AmirStmt::Assign { lhs, rhs } => {
                out.push_str(&format!("_{} = ", lhs.0));
                rhs.pretty_print_to(out, symbols, pool);
            }
            AmirStmt::Store { lhs, rhs } => {
                lhs.pretty_print_to(out, symbols, pool);
                out.push_str(" = ");
                rhs.pretty_print_to(out, symbols, pool);
            }
            AmirStmt::Call {
                lhs, callee, args, ..
            } => {
                if let Some(l) = lhs {
                    out.push_str(&format!("_{} = ", l.0));
                }
                out.push_str("call ");
                callee.pretty_print_to(out, symbols, pool);
                out.push('(');
                let arg_strs: Vec<String> = args
                    .iter()
                    .map(|a| a.to_pretty_string(symbols, pool))
                    .collect();
                out.push_str(&arg_strs.join(", "));
                out.push(')');
            }
            AmirStmt::Free(op) => {
                out.push_str(&format!("free {}", op.to_pretty_string(symbols, pool)));
            }
            AmirStmt::StorageLive(local) => {
                out.push_str(&format!("StorageLive(v{})", local.0));
            }
            AmirStmt::StorageDead(local) => {
                out.push_str(&format!("StorageDead(v{})", local.0));
            }
            AmirStmt::Destroy(place) => {
                out.push_str("destroy ");
                place.pretty_print_to(out, symbols, pool);
            }
            AmirStmt::Nop => {
                out.push_str("nop");
            }
        }
    }
}

impl AmirRvalue {
    fn pretty_print_to(&self, out: &mut String, symbols: &SymbolTable, pool: &AmirLiteralPool) {
        match self {
            AmirRvalue::Use(op) => {
                op.pretty_print_to(out, symbols, pool);
            }
            AmirRvalue::Binary { op, left, right } => {
                out.push_str(&format!(
                    "{} {}, {}",
                    binary_op_name(op),
                    left.to_pretty_string(symbols, pool),
                    right.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::Unary { op, operand } => {
                out.push_str(&format!(
                    "{} {}",
                    unary_op_name(op),
                    operand.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::FieldAccess { base, field } => {
                out.push_str(&format!(
                    "{}.{}",
                    base.to_pretty_string(symbols, pool),
                    field
                ));
            }
            AmirRvalue::StructLiteral {
                struct_symbol,
                fields,
            } => {
                let struct_name = &symbols.get(*struct_symbol).name;
                out.push_str(&format!("{struct_name} {{ "));
                let field_strs: Vec<String> = fields
                    .iter()
                    .map(|(name, op)| format!("{}: {}", name, op.to_pretty_string(symbols, pool)))
                    .collect();
                out.push_str(&field_strs.join(", "));
                out.push_str(" }");
            }
            AmirRvalue::IndexAccess { base, index } => {
                out.push_str(&format!(
                    "{}[{}]",
                    base.to_pretty_string(symbols, pool),
                    index.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::Array { items } => {
                let item_strs: Vec<String> = items
                    .iter()
                    .map(|a| a.to_pretty_string(symbols, pool))
                    .collect();
                out.push_str(&format!("[{}]", item_strs.join(", ")));
            }
            AmirRvalue::Tuple { items } => {
                let item_strs: Vec<String> = items
                    .iter()
                    .map(|a| a.to_pretty_string(symbols, pool))
                    .collect();
                out.push_str(&format!("({})", item_strs.join(", ")));
            }
            AmirRvalue::Discriminant { value } => {
                out.push_str(&format!(
                    "discriminant({})",
                    value.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::EnumPayload {
                value,
                variant,
                index,
            } => {
                let variant_name = symbols.get(*variant).name.as_str();
                out.push_str(&format!(
                    "payload({} as {}.{})",
                    value.to_pretty_string(symbols, pool),
                    variant_name,
                    index
                ));
            }
            AmirRvalue::EnumConstruct {
                variant_tag,
                payload,
            } => {
                let payload_str = match payload {
                    Some(op) => format!(", {}", op.to_pretty_string(symbols, pool)),
                    None => "".to_string(),
                };
                out.push_str(&format!("enumConstruct({}{})", variant_tag, payload_str));
            }
            AmirRvalue::Len(value) => {
                out.push_str(&format!("len({})", value.to_pretty_string(symbols, pool)));
            }
            AmirRvalue::SliceData(value) => {
                out.push_str(&format!(
                    "slice_data({})",
                    value.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::SliceView { owner, data, len } => {
                out.push_str(&format!(
                    "slice_view({}, {}, {})",
                    owner.to_pretty_string(symbols, pool),
                    data.to_pretty_string(symbols, pool),
                    len.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::SliceSubslice { slice, start, len } => {
                out.push_str(&format!(
                    "slice_subslice({}, {}, {})",
                    slice.to_pretty_string(symbols, pool),
                    start.to_pretty_string(symbols, pool),
                    len.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::StrBytes { source } => {
                out.push_str(&format!(
                    "str_bytes({})",
                    source.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::StrView { owner } => {
                out.push_str(&format!(
                    "str_view({})",
                    owner.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::Alloc(value) => {
                out.push_str(&format!("alloc({})", value.to_pretty_string(symbols, pool)));
            }
            AmirRvalue::Load(place) => {
                place.pretty_print_to(out, symbols, pool);
            }
            AmirRvalue::Borrow(place) => {
                out.push('&');
                place.pretty_print_to(out, symbols, pool);
            }
            AmirRvalue::BorrowMut(place) => {
                out.push_str("&mut ");
                place.pretty_print_to(out, symbols, pool);
            }
            AmirRvalue::RelativeBorrow { local, mutable } => {
                if *mutable {
                    out.push_str(&format!("relative_borrow_mut(s{})", local.0));
                } else {
                    out.push_str(&format!("relative_borrow(s{})", local.0));
                }
            }
            AmirRvalue::CoroutineReady {
                value,
                payload_ty,
                stack,
            } => {
                let kind = if *stack { "stack" } else { "heap" };
                out.push_str(&format!(
                    "coroutine_ready.{kind}(ty#{}, {})",
                    payload_ty.as_usize(),
                    value.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::GenInsert {
                value, payload_ty, ..
            } => {
                out.push_str(&format!(
                    "gen_insert.compiler_managed(ty#{}, {})",
                    payload_ty.as_usize(),
                    value.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::GenGet {
                gen_ref,
                payload_ty,
                ..
            } => {
                out.push_str(&format!(
                    "gen_get.compiler_managed(ty#{}, {})",
                    payload_ty.as_usize(),
                    gen_ref.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::GenSet {
                gen_ref,
                value,
                payload_ty,
                ..
            } => {
                out.push_str(&format!(
                    "gen_set.compiler_managed(ty#{}, {}, {})",
                    payload_ty.as_usize(),
                    gen_ref.to_pretty_string(symbols, pool),
                    value.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::GenUpsert {
                gen_ref,
                value,
                payload_ty,
                ..
            } => {
                out.push_str(&format!(
                    "gen_upsert.compiler_managed(ty#{}, {}, {})",
                    payload_ty.as_usize(),
                    gen_ref.to_pretty_string(symbols, pool),
                    value.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::GenRemove {
                gen_ref,
                payload_ty,
                ..
            } => {
                out.push_str(&format!(
                    "gen_remove.compiler_managed(ty#{}, {})",
                    payload_ty.as_usize(),
                    gen_ref.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::StringInterp { parts } => {
                let parts_str: Vec<String> = parts
                    .iter()
                    .map(|p| p.to_pretty_string(symbols, pool))
                    .collect();
                out.push_str(&format!("stringInterp({})", parts_str.join(", ")));
            }
            AmirRvalue::ToStr { value, src_ty } => {
                out.push_str(&format!(
                    "to_str(ty#{}, {})",
                    src_ty.as_usize(),
                    value.to_pretty_string(symbols, pool)
                ));
            }
            AmirRvalue::BlackBox { value, value_ty } => {
                out.push_str(&format!(
                    "black_box(ty#{}, {})",
                    value_ty.as_usize(),
                    value.to_pretty_string(symbols, pool)
                ));
            }
        }
    }
}

impl AmirOperand {
    fn to_pretty_string(self, symbols: &SymbolTable, pool: &AmirLiteralPool) -> String {
        let mut out = String::new();
        self.pretty_print_to(&mut out, symbols, pool);
        out
    }

    fn pretty_print_to(&self, out: &mut String, symbols: &SymbolTable, pool: &AmirLiteralPool) {
        match self {
            AmirOperand::Copy(t) => {
                out.push_str(&format!("_{}", t.0));
            }
            AmirOperand::Move(t) => {
                out.push_str(&format!("move _{}", t.0));
            }
            AmirOperand::Constant(c) => {
                c.pretty_print_to(out, pool);
            }
            AmirOperand::FunctionRef(sym) => {
                out.push_str(&format!("fn@{}", symbols.get(*sym).name));
            }
            AmirOperand::GlobalRef(sym) => {
                out.push_str(&format!("global@{}", symbols.get(*sym).name));
            }
        }
    }
}

impl AmirConstant {
    fn pretty_print_to(&self, out: &mut String, pool: &AmirLiteralPool) {
        match self {
            AmirConstant::Pool(id) => match pool.get(*id) {
                AmirLiteralEntry::Int(v) => out.push_str(v),
                AmirLiteralEntry::Float(v) => out.push_str(v),
                AmirLiteralEntry::Str(v) => out.push_str(&format!("\"{v}\"")),
                AmirLiteralEntry::Char(v) => {
                    if let Some(value) = v.chars().next() {
                        out.push_str(&arandu_lexer::char_literal(value));
                    } else {
                        out.push_str("''");
                    }
                }
            },
            AmirConstant::Bool(v) => out.push_str(&v.to_string()),
            AmirConstant::Nil => out.push_str("nil"),
        }
    }
}

fn format_args(args: &[AmirOperand], symbols: &SymbolTable, pool: &AmirLiteralPool) -> String {
    if args.is_empty() {
        return String::new();
    }
    let arg_strs: Vec<String> = args
        .iter()
        .map(|a| a.to_pretty_string(symbols, pool))
        .collect();
    format!("({})", arg_strs.join(", "))
}

impl AmirTerminator {
    pub fn pretty_print_to(&self, out: &mut String, symbols: &SymbolTable, pool: &AmirLiteralPool) {
        match self {
            AmirTerminator::Return => {
                out.push_str("return");
            }
            AmirTerminator::Goto { target, args } => {
                out.push_str(&format!(
                    "goto bb{}{}",
                    target.0,
                    format_args(args, symbols, pool)
                ));
            }
            AmirTerminator::Branch {
                condition,
                if_true,
                true_args,
                if_false,
                false_args,
            } => {
                out.push_str(&format!(
                    "branch {} => bb{}{}, else bb{}{}",
                    condition.to_pretty_string(symbols, pool),
                    if_true.0,
                    format_args(true_args, symbols, pool),
                    if_false.0,
                    format_args(false_args, symbols, pool)
                ));
            }
            AmirTerminator::SwitchInt {
                discriminant,
                targets,
                otherwise,
            } => {
                out.push_str(&format!(
                    "switchInt {} {{ ",
                    discriminant.to_pretty_string(symbols, pool)
                ));
                let target_strs: Vec<String> = targets
                    .iter()
                    .map(|(val, dest, target_args)| {
                        format!(
                            "{} => bb{}{}",
                            val,
                            dest.0,
                            format_args(target_args, symbols, pool)
                        )
                    })
                    .collect();
                out.push_str(&target_strs.join(", "));
                if !targets.is_empty() {
                    out.push_str(", ");
                }
                out.push_str(&format!(
                    "otherwise => bb{}{} }}",
                    otherwise.0.0,
                    format_args(&otherwise.1, symbols, pool)
                ));
            }
            AmirTerminator::Suspend {
                future,
                resume,
                args,
            } => {
                out.push_str(&format!(
                    "suspend {} → bb{}{}",
                    future.to_pretty_string(symbols, pool),
                    resume.0,
                    format_args(args, symbols, pool)
                ));
            }
            AmirTerminator::Unreachable => {
                out.push_str("unreachable");
            }
        }
    }
}

fn binary_op_name(op: &BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "add",
        BinaryOp::Sub => "sub",
        BinaryOp::Mul => "mul",
        BinaryOp::Div => "div",
        BinaryOp::Mod => "mod",
        BinaryOp::Equal => "eq",
        BinaryOp::NotEqual => "ne",
        BinaryOp::Lt => "lt",
        BinaryOp::LtEqual => "le",
        BinaryOp::Gt => "gt",
        BinaryOp::GtEqual => "ge",
        BinaryOp::And => "and",
        BinaryOp::Or => "or",
        BinaryOp::BitAnd => "bitand",
        BinaryOp::BitOr => "bitor",
        BinaryOp::BitXor => "bitxor",
        BinaryOp::ShiftLeft => "shl",
        BinaryOp::ShiftRight => "shr",
        BinaryOp::NullCoalesce => "null_coalesce",
        BinaryOp::RangeExclusive => "range_exclusive",
        BinaryOp::RangeInclusive => "range_inclusive",
    }
}

fn unary_op_name(op: &UnaryOp) -> &'static str {
    match op {
        UnaryOp::Not => "not",
        UnaryOp::Neg => "neg",
        UnaryOp::BitNot => "bitnot",
        UnaryOp::Await => "await",
        UnaryOp::Ref => "&",
        UnaryOp::RefMut => "&mut",
        UnaryOp::Deref => "*",
    }
}
