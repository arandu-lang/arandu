//! Type-directed AST and program synthesis.

use super::types::{
    char_literal, float_literal, ConstantValue, Rng, Ty, MAX_BASE, MAX_DEPTH, MAX_LOOP_TRIPS,
};

#[derive(Clone, Debug)]
pub struct Expr {
    pub ty: Ty,
    pub source: String,
    pub value: ConstantValue,
}

impl Expr {
    fn expected_literal(&self) -> String {
        match self.value {
            ConstantValue::Int(value) => value.to_string(),
            ConstantValue::UInt(value) => format!("{value} as uint"),
            ConstantValue::USize(value) => format!("{value} as usize"),
            ConstantValue::ISize(value) => format!("{value} as isize"),
            ConstantValue::Bool(value) => value.to_string(),
            ConstantValue::Float(value) => float_literal(value),
            ConstantValue::Char(value) => char_literal(value),
        }
    }

    fn int_value(&self) -> i64 {
        match self.value {
            ConstantValue::Int(value) => value,
            _ => unreachable!("integer expression has non-integer value"),
        }
    }

    fn uint_value(&self) -> u64 {
        match self.value {
            ConstantValue::UInt(value) => value,
            _ => unreachable!("unsigned expression has non-unsigned value"),
        }
    }

    fn usize_value(&self) -> u64 {
        match self.value {
            ConstantValue::USize(value) => value,
            _ => unreachable!("usize expression has non-usize value"),
        }
    }

    fn isize_value(&self) -> i64 {
        match self.value {
            ConstantValue::ISize(value) => value,
            _ => unreachable!("isize expression has non-isize value"),
        }
    }

    fn bool_value(&self) -> bool {
        match self.value {
            ConstantValue::Bool(value) => value,
            _ => unreachable!("boolean expression has non-boolean value"),
        }
    }

    fn float_value(&self) -> f64 {
        match self.value {
            ConstantValue::Float(value) => value,
            _ => unreachable!("float expression has non-float value"),
        }
    }

    fn char_value(&self) -> char {
        match self.value {
            ConstantValue::Char(value) => value,
            _ => unreachable!("char expression has non-char value"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SynthesizedProgram {
    pub source: String,
    pub expected_result: i32,
}

#[derive(Clone, Debug)]
pub struct SynthesizedPrefix {
    pub base: u64,
    pub base_value: i64,
    pub left: Expr,
    pub unsigned: Expr,
    pub condition: Expr,
}

#[allow(dead_code)]
pub fn synthesize(seed: u64) -> String {
    synthesize_with_oracle(seed).source
}

pub fn synthesize_with_oracle(seed: u64) -> SynthesizedProgram {
    let mut rng = Rng::new(seed);
    let SynthesizedPrefix {
        base,
        base_value,
        left,
        unsigned,
        condition,
    } = gen_synthesized_prefix(&mut rng);
    let right = gen_expr(&mut rng, Ty::Int, MAX_DEPTH, base_value);
    let float_expr = gen_expr(&mut rng, Ty::Float, MAX_DEPTH, base_value);
    let char_expr = gen_expr(&mut rng, Ty::Char, MAX_DEPTH, base_value);
    let usize_expr = gen_expr(&mut rng, Ty::USize, MAX_DEPTH, base_value);
    let isize_expr = gen_expr(&mut rng, Ty::ISize, MAX_DEPTH, base_value);
    let (string_expr, expected_string) = gen_string_expr(&mut rng);
    let triple_candidates = [
        &left,
        &unsigned,
        &usize_expr,
        &isize_expr,
        &condition,
        &float_expr,
        &char_expr,
    ];
    let pair_start = rng.below(triple_candidates.len() as u64) as usize;
    let pair_values = [
        triple_candidates[pair_start],
        triple_candidates[(pair_start + 1) % triple_candidates.len()],
    ];
    let pair_type_args = pair_values
        .iter()
        .map(|expression| expression.ty.source_name())
        .collect::<Vec<_>>()
        .join(", ");
    let pair_arguments = pair_values
        .iter()
        .map(|expression| {
            format!(
                "identity<{}>({})",
                expression.ty.source_name(),
                expression.source
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let pair_guards = pair_values
        .iter()
        .zip(["pair_value", "pair_enabled"])
        .map(|(expression, binding)| format!("{binding} != {}", expression.expected_literal()))
        .collect::<Vec<_>>()
        .join(" || ");
    let pair_check = format!(
        "let pair_value, pair_enabled = make_pair<{pair_type_args}>({pair_arguments})\n    if {pair_guards} {{ return -1000041 }}"
    );
    let triple_start = rng.below(triple_candidates.len() as u64) as usize;
    let triple_values: [&Expr; 3] = std::array::from_fn(|offset| {
        triple_candidates[(triple_start + offset) % triple_candidates.len()]
    });
    let triple_type_args = triple_values
        .iter()
        .map(|expression| expression.ty.source_name())
        .collect::<Vec<_>>()
        .join(", ");
    let triple_arguments = triple_values
        .iter()
        .map(|expression| {
            format!(
                "identity<{}>({})",
                expression.ty.source_name(),
                expression.source
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let triple_bindings = ["triple_value", "triple_enabled", "triple_character"];
    let triple_destructuring = format!(
        "let {}, {}, {} = make_triple<{triple_type_args}>({triple_arguments})",
        triple_bindings[0], triple_bindings[1], triple_bindings[2]
    );
    let triple_guards = triple_values
        .iter()
        .zip(triple_bindings)
        .map(|(expression, binding)| format!("{binding} != {}", expression.expected_literal()))
        .collect::<Vec<_>>()
        .join(" || ");
    let triple_check =
        format!("{triple_destructuring}\n    if {triple_guards} {{ return -1000042 }}");

    debug_assert_eq!(left.ty, Ty::Int);
    debug_assert_eq!(unsigned.ty, Ty::UInt);
    debug_assert_eq!(condition.ty, Ty::Bool);
    debug_assert_eq!(right.ty, Ty::Int);
    debug_assert_eq!(float_expr.ty, Ty::Float);
    debug_assert_eq!(char_expr.ty, Ty::Char);

    let loop_iterations =
        i64::try_from(base % (MAX_LOOP_TRIPS + 1)).expect("loop bound fits in i64");
    let loop_sum = loop_iterations * (loop_iterations + 1) / 2;
    let loop_source = if seed & 1 == 0 {
        format!(
            "let mut remaining: int = base % {loop_bound}\n    let mut total: int = 0\n    while remaining > 0 {{\n        total = total + remaining\n        remaining = remaining - 1\n    }}",
            loop_bound = MAX_LOOP_TRIPS + 1,
        )
    } else {
        format!(
            "let mut total: int = 0\n    let mut loop_index: int = 1\n    for loop_index = 1; loop_index <= {loop_iterations}; loop_index += 1 {{\n        total = total + loop_index\n    }}"
        )
    };
    // The vector and borrow checks guarantee the generated operations do not
    // alter these values. The two adjust calls cancel their +/-2 deltas.
    let expected_result = match base % 3 {
        0 => {
            if condition.bool_value() {
                1
            } else {
                0
            }
        }
        1 => i32::try_from(3 * right.int_value() + base_value % 9)
            .expect("bounded synthesized result fits in i32"),
        _ => i32::try_from(left.int_value() + loop_sum)
            .expect("bounded synthesized result fits in i32"),
    };
    let source = format!(
        "import io\nimport std.alloc.vec as vec\nimport std.core.slice as slice\nimport std.core.result as core_result\n\nstruct Sample {{\n    left: int\n    right: int\n    enabled: bool\n}}\n\nenum Choice {{\n    Left(int),\n    Right(int),\n    Flag(bool),\n}}\n\nfunc identity<T>(value: T): T {{\n    return value\n}}\n\nfunc read_ref(value: ref int): int {{\n    return *value\n}}\n\nfunc read_exclusive(value: mut ref int): int {{\n    return *value\n}}\n\nfunc adjust(value: int, enabled: bool): int {{\n    if enabled {{ return value + 2 }}\n    return value - 2\n}}\n\nfunc main(): int {{\n    let base: int = {base}\n    let float_base: float = {base}.0\n    let float_result: float = identity<float>({float_source})\n    if float_result != {float_expected} {{ return -1000000 }}\n    let values = [identity<int>({}), identity<int>({}), base]\n    let index: int = base % 3\n    {loop_source}\n    let sample = Sample {{ left: values[index], right: values[(index + 1) % 3] + total, enabled: identity<bool>({}) }}\n    let mut borrow_target: int = sample.left\n    let shared_value = read_ref(ref borrow_target)\n    let exclusive_value = read_exclusive(mut ref borrow_target)\n    let mut dynamic = vec.Vec<int>.new()\n    if !dynamic.tryReserve(1 as usize) {{\n        return -1000001\n    }}\n    if dynamic.capacity() < 1 as usize || !dynamic.isEmpty() {{\n        return -1000002\n    }}\n    let mut dynamic_count: int = 0\n    while dynamic_count < 9 {{\n        if !dynamic.tryPush(sample.left + dynamic_count) {{\n            return -1000003\n        }}\n        dynamic_count = dynamic_count + 1\n    }}\n    let dynamic_len = dynamic.len() as int\n    let dynamic_view_len = slice.len<int>(dynamic.asSlice()) as int\n    if dynamic_view_len != dynamic_len {{\n        return -1000004\n    }}\n    let out_of_bounds = dynamic.getCopy(dynamic_len as usize)\n    let out_of_bounds_has_value = match out_of_bounds {{\n        Some(_) => true\n        None => false\n    }}\n    if out_of_bounds_has_value {{\n        return -1000005\n    }}\n    if dynamic.set(dynamic_len as usize, -1) {{\n        return -1000006\n    }}\n    if dynamic_len != 9 {{\n        return -1000007\n    }}\n    let dynamic_index = (base % dynamic_len) as usize\n    let dynamic_element = dynamic.getCopy(dynamic_index)\n    let dynamic_value = match dynamic_element {{\n        Some(value) => value\n        None => 0\n    }}\n    let expected_dynamic_value = sample.left + dynamic_index as int\n    if dynamic_value != expected_dynamic_value {{\n        return -1000008\n    }}\n    let dynamic_last = dynamic.getCopy(8 as usize)\n    let dynamic_tail = match dynamic_last {{\n        Some(value) => value\n        None => 0\n    }}\n    if dynamic_tail != sample.left + 8 {{\n        return -1000009\n    }}\n    if !dynamic.set(8 as usize, sample.right) {{\n        return -1000010\n    }}\n    let updated_last = dynamic.getCopy(8 as usize)\n    let updated_value = match updated_last {{\n        Some(value) => value\n        None => 0\n    }}\n    if updated_value != sample.right {{\n        return -1000011\n    }}\n    let popped = dynamic.pop()\n    let popped_value = match popped {{\n        Some(value) => value\n        None => 0\n    }}\n    if popped_value != sample.right || dynamic.len() != 8 as usize {{\n        return -1000012\n    }}\n    dynamic.clear()\n    let empty_pop = dynamic.pop()\n    let empty_pop_has_value = match empty_pop {{\n        Some(_) => true\n        None => false\n    }}\n    if empty_pop_has_value {{\n        return -1000013\n    }}\n    let adjusted = adjust(shared_value, sample.enabled) + adjust(exclusive_value, !sample.enabled) + dynamic_value\n    let selected = if base % 3 == 0 {{ Choice.Flag(sample.enabled) }} else if base % 3 == 1 {{ Choice.Left(adjusted) }} else {{ Choice.Right(sample.right) }}\n    let result = match selected {{\n        Choice.Left(value) => value\n        Choice.Right(value) => value\n        Choice.Flag(enabled) => if enabled {{ 1 }} else {{ 0 }}\n    }}\n    io.println(result.to_str())\n    return result\n}}\n",
        left.source,
        right.source,
        condition.source,
        loop_source = loop_source,
        float_source = float_expr.source,
        float_expected = float_literal(float_expr.float_value())
    );
    let source = replace_once(
        source,
        "enum Choice {\n    Left(int),\n    Right(int),\n    Flag(bool),\n}",
        "enum Choice {\n    Left(int),\n    Right(int),\n    Flag(bool),\n}\n\nstruct OwnedPack {\n    items: vec.Vec<int>\n    marker: int\n}",
    );
    let source = replace_once(
        source,
        "func read_ref(value: ref int): int {",
        concat!(
            "func make_owned_pack(value: int, marker: int): OwnedPack {\n",
            "    let mut items = vec.Vec<int>.new()\n",
            "    items.push(value)\n",
            "    return OwnedPack { items: items, marker: marker }\n",
            "}\n\n",
            "func read_ref(value: ref int): int {"
        ),
    );
    let sample_enabled = format!("enabled: identity<bool>({}) }}", condition.source);
    let sample_with_char = format!(
        "enabled: identity<bool>({}), character: identity<char>(generated_character) }}",
        condition.source
    );
    let source = replace_once(
        source,
        "    enabled: bool\n}",
        "    enabled: bool\n    character: char\n}",
    );
    let source = replace_once(
        source,
        "    Flag(bool),\n}",
        "    Flag(bool),\n    Letter(char),\n}",
    );
    let source = replace_once(
        source,
        "        Choice.Flag(enabled) => if enabled { 1 } else { 0 }\n    }",
        "        Choice.Flag(enabled) => if enabled { 1 } else { 0 }\n        Choice.Letter(character) => if character == sample.character { 1 } else { -73 }\n    }",
    );
    let source = replace_once(
        source,
        "    let sample = Sample {",
        &format!(
            "    let generated_character: char = {}\n    let sample = Sample {{",
            char_expr.source
        ),
    );
    let source = replace_once(source, &sample_enabled, &sample_with_char);
    let source = replace_once(
        source,
        "\n    let mut borrow_target:",
        &format!(
            "\n    if sample.character != {} {{ return -1000014 }}\n    let mut borrow_target:",
            char_literal(char_expr.char_value())
        ),
    );
    let source = replace_once(
        source,
        "    let mut borrow_target:",
        concat!(
            "    let owned_pack = transfer<OwnedPack>(make_owned_pack(sample.left, sample.right))\n",
            "    if owned_pack.marker != sample.right || owned_pack.items.len() != 1 as usize { return -1000053 }\n",
            "    let packed_value = owned_pack.items.getCopy(0 as usize)\n",
            "    let recovered_packed_value = match packed_value {\n",
            "        Some(value) => value\n",
            "        None => 0\n",
            "    }\n",
            "    if recovered_packed_value != sample.left { return -1000054 }\n",
            "    let mut borrow_target:"
        ),
    );
    let source = replace_once(
        source,
        "    let float_base: float =",
        &format!(
            "    let uint_base: uint = {base} as uint\n    let uint_result: uint = identity<uint>({})\n    if uint_result != {} as uint {{ return -1000015 }}\n    let float_base: float =",
            unsigned.source,
            unsigned.uint_value()
        ),
    );
    let source = replace_once(
        source,
        "func read_ref(value: ref int): int {",
        concat!(
            "func make_pair<T, U>(left: T, enabled: U): (T, U) {\n",
            "    return left, enabled\n",
            "}\n\n",
            "func make_triple<T, U, V>(left: T, enabled: U, character: V): (T, U, V) {\n",
            "    return left, enabled, character\n",
            "}\n\n",
            "func transfer<T>(own value: T): T {\n",
            "    return value\n",
            "}\n\n",
            "func relay<T>(own value: T): T {\n",
            "    return value\n",
            "}\n\n",
            "func make_vec<T>(): vec.Vec<T> {\n",
            "    return vec.Vec<T>.new()\n",
            "}\n\n",
            "func read_ref(value: ref int): int {"
        ),
    );
    let source = replace_once(
        source,
        "    let float_result: float =",
        &format!("    {pair_check}\n    {triple_check}\n    let float_result: float ="),
    );
    let source = replace_once(
        source,
        "let mut dynamic = vec.Vec<int>.new()",
        "let mut dynamic = transfer<vec.Vec<int>>(relay<vec.Vec<int>>(make_vec<int>()))",
    );
    let source = replace_once(
        source,
        "    let adjusted =",
        concat!(
            "    let mut flags = transfer<vec.Vec<bool>>(make_vec<bool>())\n",
            "    if !flags.tryReserve(1 as usize) {\n",
            "        return -1000016\n",
            "    }\n",
            "    if !flags.tryPush(sample.enabled) {\n",
            "        return -1000017\n",
            "    }\n",
            "    let flags_view_len = slice.len<bool>(flags.asSlice())\n",
            "    let first_flag = flags.getCopy(0 as usize)\n",
            "    let first_flag_value = match first_flag {\n",
            "        Some(value) => value\n",
            "        None => false\n",
            "    }\n",
            "    if flags_view_len != 1 as usize || first_flag_value != sample.enabled {\n",
            "        return -1000018\n",
            "    }\n",
            "    let mut choices = transfer<vec.Vec<Choice>>(make_vec<Choice>())\n",
            "    if !choices.tryPush(Choice.Flag(sample.enabled)) {\n",
            "        return -1000019\n",
            "    }\n",
            "    let selected_choice = choices.pop()\n",
            "    let recovered_flag = match selected_choice {\n",
            "        Some(Choice.Flag(value)) => value\n",
            "        Some(_) => false\n",
            "        None => false\n",
            "    }\n",
            "    if recovered_flag != sample.enabled {\n",
            "        return -1000020\n",
            "    }\n",
            "    if !choices.tryPush(Choice.Letter(sample.character)) {\n",
            "        return -1000021\n",
            "    }\n",
            "    let selected_character = choices.pop()\n",
            "    let recovered_character = match selected_character {\n",
            "        Some(Choice.Letter(value)) => value\n",
            "        Some(_) => 'a'\n",
            "        None => 'a'\n",
            "    }\n",
            "    if recovered_character != sample.character {\n",
            "        return -1000022\n",
            "    }\n",
            "    let mut characters = transfer<vec.Vec<char>>(make_vec<char>())\n",
            "    if !characters.tryReserve(1 as usize) {\n",
            "        return -1000023\n",
            "    }\n",
            "    if !characters.tryPush(sample.character) {\n",
            "        return -1000024\n",
            "    }\n",
            "    let characters_view_len = slice.len<char>(characters.asSlice())\n",
            "    let first_character = characters.getCopy(0 as usize)\n",
            "    let recovered_vector_character = match first_character {\n",
            "        Some(value) => value\n",
            "        None => 'a'\n",
            "    }\n",
            "    if characters_view_len != 1 as usize || recovered_vector_character != sample.character {\n",
            "        return -1000025\n",
            "    }\n",
            "    let mut floats = transfer<vec.Vec<float>>(make_vec<float>())\n",
            "    if !floats.tryReserve(1 as usize) {\n",
            "        return -1000026\n",
            "    }\n",
            "    if !floats.tryPush(float_result) {\n",
            "        return -1000027\n",
            "    }\n",
            "    let first_float = floats.getCopy(0 as usize)\n",
            "    let recovered_vector_float = match first_float {\n",
            "        Some(value) => value\n",
            "        None => 0.0\n",
            "    }\n",
            "    if recovered_vector_float != float_result {\n",
            "        return -1000028\n",
            "    }\n",
            "    let mut unsigned_values = transfer<vec.Vec<uint>>(relay<vec.Vec<uint>>(make_vec<uint>()))\n",
            "    if !unsigned_values.tryReserve(1 as usize) {\n",
            "        return -1000045\n",
            "    }\n",
            "    if !unsigned_values.tryPush(uint_result) {\n",
            "        return -1000046\n",
            "    }\n",
            "    let unsigned_view_len = slice.len<uint>(unsigned_values.asSlice())\n",
            "    let first_unsigned = unsigned_values.getCopy(0 as usize)\n",
            "    let recovered_unsigned = match first_unsigned {\n",
            "        Some(value) => value\n",
            "        None => 0 as uint\n",
            "    }\n",
            "    if unsigned_view_len != 1 as usize || recovered_unsigned != uint_result {\n",
            "        return -1000047\n",
            "    }\n",
            "    let popped_unsigned = unsigned_values.pop()\n",
            "    let recovered_popped_unsigned = match popped_unsigned {\n",
            "        Some(value) => value\n",
            "        None => 0 as uint\n",
            "    }\n",
            "    if recovered_popped_unsigned != uint_result || unsigned_values.len() != 0 as usize {\n",
            "        return -1000048\n",
            "    }\n",
            "    let mut cap_vec = vec.Vec<int>.withCapacity(16 as usize)\n",
            "    let cap_vec_init_ok = cap_vec.capacity() >= (16 as usize) && cap_vec.isEmpty() && cap_vec.len() == (0 as usize)\n",
            "    cap_vec.push(sample.right)\n",
            "    cap_vec.push(sample.left)\n",
            "    let cap_vec_push_ok = !cap_vec.isEmpty() && cap_vec.len() == (2 as usize)\n",
            "    cap_vec.clear()\n",
            "    let cap_vec_clear_ok = cap_vec.isEmpty() && cap_vec.len() == (0 as usize) && cap_vec.capacity() >= (16 as usize)\n",
            "    if !cap_vec_init_ok || !cap_vec_push_ok || !cap_vec_clear_ok {\n",
            "        return -1000059\n",
            "    }\n",
            "    let mut result_values = transfer<vec.Vec<core_result.Result<int, bool>>>(relay<vec.Vec<core_result.Result<int, bool>>>(make_vec<core_result.Result<int, bool>>()))\n",
            "    if !result_values.tryReserve(2 as usize) {\n",
            "        return -1000049\n",
            "    }\n",
            "    if !result_values.tryPush(Result.Ok(sample.left)) {\n",
            "        return -1000050\n",
            "    }\n",
            "    if !result_values.tryPush(Result.Err(sample.enabled)) {\n",
            "        return -1000051\n",
            "    }\n",
            "    let nested_view_len = slice.len<core_result.Result<int, bool>>(result_values.asSlice())\n",
            "    let first_nested_result = result_values.getCopy(0 as usize)\n",
            "    let first_nested_valid = match first_nested_result {\n",
            "        Some(value) => value.isOk() && value.unwrapOr(-1) == sample.left\n",
            "        None => false\n",
            "    }\n",
            "    let second_nested_result = result_values.pop()\n",
            "    let second_nested_valid = match second_nested_result {\n",
            "        Some(value) => value.isErr() && value.unwrapOr(-1) == -1\n",
            "        None => false\n",
            "    }\n",
            "    if nested_view_len != 2 as usize || !first_nested_valid || !second_nested_valid || result_values.len() != 1 as usize {\n",
            "        return -1000052\n",
            "    }\n",
            "    let adjusted ="
        ),
    );
    let result_checks = "    let mut boxed: core_result.Result<int, bool> = Result.Ok(result)\n    if !sample.enabled { boxed = Result.Err(false) }\n    let boxed_is_ok = boxed.isOk()\n    let boxed_is_err = boxed.isErr()\n    if boxed_is_ok == boxed_is_err { return -1000029 }\n    if sample.enabled && !boxed_is_ok { return -1000030 }\n    if !sample.enabled && !boxed_is_err { return -1000031 }\n    let boxed_value = boxed.unwrapOr(result + 1)\n    let expected_boxed_value = if sample.enabled { result } else { result + 1 }\n    if boxed_value != expected_boxed_value { return -1000032 }\n    let boxed_character_ok: core_result.Result<char, bool> = Result.Ok(sample.character)\n    if !boxed_character_ok.isOk() || boxed_character_ok.isErr() { return -1000033 }\n    let recovered_ok_character = boxed_character_ok.unwrapOr('q')\n    if recovered_ok_character != sample.character { return -1000034 }\n    let boxed_character_err: core_result.Result<char, bool> = Result.Err(false)\n    if boxed_character_err.isOk() || !boxed_character_err.isErr() { return -1000035 }\n    let recovered_err_character = boxed_character_err.unwrapOr('q')\n    if recovered_err_character != 'q' { return -1000036 }\n    let boxed_int_char_ok: core_result.Result<int, char> = Result.Ok(result)\n    if !boxed_int_char_ok.isOk() || boxed_int_char_ok.isErr() { return -1000037 }\n    if boxed_int_char_ok.unwrapOr(result + 1) != result { return -1000038 }\n    let boxed_int_char_err: core_result.Result<int, char> = Result.Err(sample.character)\n    if boxed_int_char_err.isOk() || !boxed_int_char_err.isErr() { return -1000039 }\n    if boxed_int_char_err.unwrapOr(result + 1) != result + 1 { return -1000040 }\n    if result % 2 == 0 {\n        io.println(\"result-even\")\n        io.eprint(\"result-even\")\n    } else {\n        io.println(\"result-odd\")\n        io.eprint(\"result-odd\")\n    }";
    let source = replace_once(source, "    io.println(result.to_str())", result_checks);
    let source = replace_once(
        source,
        "import io\nimport std.alloc.vec as vec",
        "import io\nimport std.env as env\nimport std.alloc.vec as vec",
    );
    let source = replace_once(
        source,
        "    let base: int = ",
        &format!(
            "    if env.argsLen() != 3 {{ return -1000029 }}\n    if env.arg(0) != \"arandu-smith\" {{ return -1000043 }}\n    if env.arg(1) != \"\" || env.arg(2) != \"argument-two\" {{ return -1000030 }}\n    if env.arg(3) != \"\" || env.arg(-1) != \"\" {{ return -1000044 }}\n    let generated_text: str = {string_expr}\n    if generated_text != \"{expected_string}\" {{ return -1000031 }}\n    let base: int = ",
        ),
    );
    let source = add_seeded_hash_map_case(source, seed, base_value);
    let source = add_seeded_bitset_case(source, seed);
    let source = add_seeded_try_operator_case(source, seed, base_value);
    let source = add_seeded_slice_iter_case(source, seed, base_value);
    let source = add_seeded_option_result_cmp_case(source, seed, base_value);
    let source = add_seeded_str_mem_case(source, seed, base_value);
    // Keep the normal independent result/stdout oracle unchanged. A bounded
    // static tree checks parent-first exclusion in real Smith runs, not only
    // in a separate hand-picked unit fixture.
    let (static_body, static_expected) = gen_comptime_if_node(&mut Rng::new(seed), 0, 3);
    let source = replace_once(source, "func main(): int {", &format!(
        "func smith_static_tree(): int {{\n{static_body}\n}}\n\nfunc main(): int {{\n    if smith_static_tree() != {static_expected} {{ return -1000060 }}"
    ));
    SynthesizedProgram {
        source,
        expected_result,
    }
}

/// Add a seed-varying, collision-heavy HashMap case to one of every 64 generated
/// programs. The custom key keeps the Robin Hood probe sequence deterministic;
/// the generated base and values vary independently with the seed.
fn add_seeded_hash_map_case(source: String, seed: u64, base: i64) -> String {
    if !seed.is_multiple_of(64) {
        return source;
    }

    let value_base = i64::try_from(seed % 997).expect("bounded HashMap value seed");
    let mut keys = (0..9_i64)
        .map(|index| base * 8 + index * 8)
        .collect::<Vec<_>>();
    keys[5] = seeded_hash_collision_key(keys[0], &keys);
    let values = (0..9_i64)
        .map(|index| value_base + 100 + index)
        .collect::<Vec<_>>();
    let declarations = concat!(
        "struct GeneratedKey {\n",
        "    value: int\n",
        "}\n\n",
        "func GeneratedKey.eq(self: ref GeneratedKey, other: ref GeneratedKey): bool {\n",
        "    return self.value == other.value\n",
        "}\n\n",
        "func GeneratedKey.hash<H: hash.Hasher>(self: ref GeneratedKey, state: mut ref H): void {\n",
        "    state.writeInt(self.value)\n",
        "}\n\n",
        "func generated_map_value_or_minus_one(table: ref hash_map.HashMap<GeneratedKey, int>, key: ref GeneratedKey): int {\n",
        "    match table.get(key) {\n",
        "        Some(value) => { return value }\n",
        "        None => { return -1 }\n",
        "    }\n",
        "}\n\n",
    );
    let source = replace_once(
        source,
        "import std.alloc.vec as vec",
        "import std.alloc.vec as vec\nimport std.alloc.hash_map as hash_map\nimport std.core.hash as hash",
    );
    let source = replace_once(
        source,
        "struct Sample {",
        &format!("{declarations}struct Sample {{"),
    );

    let mut scenario = format!(
        "    let generated_map_key0 = GeneratedKey {{ value: {} }}\n",
        keys[0]
    );
    for (index, key) in keys.iter().enumerate().skip(1) {
        scenario.push_str(&format!(
            "    let generated_map_key{index} = GeneratedKey {{ value: {key} }}\n"
        ));
    }
    scenario.push_str(
        "    let mut generated_map_pre = hash_map.HashMap<GeneratedKey, int>.withCapacity(16 as usize)\n",
    );
    scenario.push_str("    let map_pre_cap_ok = generated_map_pre.capacity() == 16 as usize\n");
    scenario.push_str("    let map_pre_empty_ok = generated_map_pre.isEmpty() && generated_map_pre.len() == 0 as usize\n");
    scenario.push_str(&format!(
        "    generated_map_pre.put(generated_map_key0, {})\n",
        values[0]
    ));
    scenario.push_str("    let map_pre_contains_ok = generated_map_pre.contains(ref generated_map_key0) && !generated_map_pre.contains(ref generated_map_key1)\n");
    scenario.push_str("    let mut generated_map = hash_map.HashMap<GeneratedKey, int>.new()\n");
    scenario.push_str("    let map_hashes_collide = ((hash_map.hashKey<GeneratedKey>(ref generated_map_key0) as uint) & 7) == ((hash_map.hashKey<GeneratedKey>(ref generated_map_key5) as uint) & 7)\n");
    for (index, value) in values.iter().enumerate().take(6) {
        scenario.push_str(&format!(
            "    generated_map.put(generated_map_key{index}, {})\n",
            value
        ));
    }
    scenario.push_str("    let map_initial_capacity_ok = generated_map.capacity() == 8 as usize\n");
    scenario.push_str("    let map_initial_len_ok = generated_map.len() == 6 as usize\n");
    scenario.push_str(&format!(
        "    let map_previous = generated_map.put(generated_map_key2, {})\n",
        values[2] + 900
    ));
    scenario.push_str(
        "    let map_previous_ok = match map_previous {\n        Some(value) => value == ",
    );
    scenario.push_str(&values[2].to_string());
    scenario.push_str("\n        None => false\n    }\n");
    scenario.push_str(&format!(
        "    let map_replacement_ok = generated_map_value_or_minus_one(ref generated_map, ref generated_map_key2) == {}\n",
        values[2] + 900
    ));
    scenario.push_str("    let map_removed_head = generated_map.remove(ref generated_map_key0)\n");
    scenario.push_str(
        "    let map_head_shift_ok = match map_removed_head {\n        Some(value) => value == ",
    );
    scenario.push_str(&values[0].to_string());
    scenario.push_str(
        " && generated_map_value_or_minus_one(ref generated_map, ref generated_map_key1) == ",
    );
    scenario.push_str(&values[1].to_string());
    scenario.push_str(
        " && generated_map_value_or_minus_one(ref generated_map, ref generated_map_key5) == ",
    );
    scenario.push_str(&values[5].to_string());
    scenario.push_str("\n        None => false\n    }\n");
    scenario
        .push_str("    let map_removed_middle = generated_map.remove(ref generated_map_key3)\n");
    scenario.push_str("    let map_middle_shift_ok = match map_removed_middle {\n        Some(value) => value == ");
    scenario.push_str(&values[3].to_string());
    scenario.push_str(
        " && generated_map_value_or_minus_one(ref generated_map, ref generated_map_key4) == ",
    );
    scenario.push_str(&values[4].to_string());
    scenario.push_str("\n        None => false\n    }\n");
    for (index, value) in values.iter().enumerate().take(9).skip(6) {
        scenario.push_str(&format!(
            "    generated_map.put(generated_map_key{index}, {})\n",
            value
        ));
    }
    scenario.push_str("    let map_growth_ok = generated_map.capacity() == 16 as usize && generated_map.len() == 7 as usize\n");
    scenario.push_str(&format!(
        "    let map_tail_ok = generated_map.contains(ref generated_map_key8) && generated_map_value_or_minus_one(ref generated_map, ref generated_map_key8) == {}\n",
        values[8]
    ));
    scenario.push_str("    generated_map.clear()\n");
    scenario.push_str("    let map_clear_ok = generated_map.isEmpty() && generated_map_value_or_minus_one(ref generated_map, ref generated_map_key5) == -1 && generated_map.capacity() == 16 as usize\n");
    scenario.push_str(&format!(
        "    generated_map.put(generated_map_key2, {})\n",
        values[2]
    ));
    scenario.push_str(&format!(
        "    let map_reuse_ok = generated_map_value_or_minus_one(ref generated_map, ref generated_map_key2) == {}\n",
        values[2]
    ));
    scenario.push_str("    if !map_hashes_collide || !map_pre_cap_ok || !map_pre_empty_ok || !map_pre_contains_ok || !map_initial_capacity_ok || !map_initial_len_ok || !map_previous_ok || !map_replacement_ok || !map_head_shift_ok || !map_middle_shift_ok || !map_growth_ok || !map_tail_ok || !map_clear_ok || !map_reuse_ok { return -1000055 }\n");

    replace_once(
        source,
        "    let base: int = ",
        &format!("{scenario}    let base: int = "),
    )
}

pub(super) fn seeded_hash_collision_key(value: i64, existing: &[i64]) -> i64 {
    let target_bucket = int_hash_bucket(value);
    let start = value.saturating_add(1);
    (start..start.saturating_add(4096))
        .find(|candidate| {
            !existing.contains(candidate) && int_hash_bucket(*candidate) == target_bucket
        })
        .expect("a distinct i32 hash collision exists in the bounded search window")
}

pub(super) fn int_hash_bucket(value: i64) -> u8 {
    let value = value as u32;
    let mut hash = 14695981039346656037_u64;
    for shift in [0, 8, 16, 24] {
        let byte = ((value >> shift) & 255) as u64;
        hash = (hash ^ byte).wrapping_mul(1099511628211);
    }
    (hash & 7) as u8
}

/// Add a seed-varying BitSet scenario beside the generated HashMap case.
/// Fixed positions straddle 64-bit word boundaries; the final position varies
/// by seed while staying within a small, allocation-safe range.
fn add_seeded_bitset_case(source: String, seed: u64) -> String {
    if !seed.is_multiple_of(64) {
        return source;
    }

    let seed_bit = 128 + seed % 63;
    let source = replace_once(
        source,
        "import std.alloc.vec as vec",
        "import std.alloc.vec as vec\nimport std.alloc.bitset as bitset",
    );
    let scenario = format!(
        concat!(
            "    let mut generated_bits = bitset.BitSet.new()\n",
            "    let generated_bit0 = generated_bits.insert(0 as usize)\n",
            "    let generated_bit63 = generated_bits.insert(63 as usize)\n",
            "    let generated_bit64 = generated_bits.insert(64 as usize)\n",
            "    let generated_bit127 = generated_bits.insert(127 as usize)\n",
            "    let generated_bit_seed = generated_bits.insert({seed_bit} as usize)\n",
            "    let generated_bit_duplicate = generated_bits.insert(64 as usize)\n",
            "    let generated_bit_membership = generated_bits.contains(0 as usize) && generated_bits.contains(63 as usize) && generated_bits.contains(64 as usize) && generated_bits.contains(127 as usize) && generated_bits.contains({seed_bit} as usize)\n",
            "    let generated_bit_count = generated_bits.countOnes() == 5 as uint\n",
            "    let generated_bit_removed = generated_bits.remove(64 as usize)\n",
            "    let generated_bit_shift_safe = !generated_bits.contains(64 as usize) && generated_bits.contains(63 as usize) && generated_bits.contains(127 as usize) && generated_bits.countOnes() == 4 as uint\n",
            "    let generated_bit_missing_remove = generated_bits.remove(64 as usize)\n",
            "    generated_bits.clear()\n",
            "    let generated_bit_clear = generated_bits.countOnes() == 0 as uint && !generated_bits.contains(127 as usize)\n",
            "    let generated_bit_reuse = generated_bits.insert({seed_bit} as usize) && generated_bits.contains({seed_bit} as usize) && generated_bits.countOnes() == 1 as uint\n",
            "    let mut bs_cap = bitset.BitSet.withCapacity(192 as usize)\n",
            "    let bs_cap_initial = bs_cap.countOnes() == 0 as uint && !bs_cap.contains(0 as usize) && !bs_cap.contains(128 as usize)\n",
            "    bs_cap.insert(1 as usize)\n",
            "    bs_cap.insert(64 as usize)\n",
            "    bs_cap.insert(190 as usize)\n",
            "    let mut bs_other = bitset.BitSet.new()\n",
            "    bs_other.insert(64 as usize)\n",
            "    bs_other.insert(128 as usize)\n",
            "    let mut bs_union = bitset.BitSet.new()\n",
            "    bs_union.unionWith(ref bs_cap)\n",
            "    bs_union.unionWith(ref bs_other)\n",
            "    let bs_union_ok = bs_union.countOnes() == 4 as uint && bs_union.contains(1 as usize) && bs_union.contains(64 as usize) && bs_union.contains(128 as usize) && bs_union.contains(190 as usize)\n",
            "    let mut bs_inter = bitset.BitSet.new()\n",
            "    bs_inter.unionWith(ref bs_cap)\n",
            "    bs_inter.intersectWith(ref bs_other)\n",
            "    let bs_inter_ok = bs_inter.countOnes() == 1 as uint && bs_inter.contains(64 as usize) && !bs_inter.contains(1 as usize) && !bs_inter.contains(128 as usize)\n",
            "    let mut bs_diff = bitset.BitSet.new()\n",
            "    bs_diff.unionWith(ref bs_cap)\n",
            "    bs_diff.differenceWith(ref bs_other)\n",
            "    let bs_diff_ok = bs_diff.countOnes() == 2 as uint && bs_diff.contains(1 as usize) && bs_diff.contains(190 as usize) && !bs_diff.contains(64 as usize)\n",
            "    if !generated_bit0 || !generated_bit63 || !generated_bit64 || !generated_bit127 || !generated_bit_seed || generated_bit_duplicate || !generated_bit_membership || !generated_bit_count || !generated_bit_removed || !generated_bit_shift_safe || generated_bit_missing_remove || !generated_bit_clear || !generated_bit_reuse || !bs_cap_initial || !bs_union_ok || !bs_inter_ok || !bs_diff_ok {{ return -1000056 }}\n",
        ),
        seed_bit = seed_bit,
    );
    replace_once(
        source,
        "    let base: int = ",
        &format!("{scenario}    let base: int = "),
    )
}

/// Add a seed-varying scenario exercising the `?` try-operator, pattern matching,
/// and null coalescing (`??`).
fn add_seeded_try_operator_case(source: String, seed: u64, base: i64) -> String {
    if seed % 64 != 32 {
        return source;
    }

    let declarations = concat!(
        "enum GeneratedStepError {\n",
        "    StepFailed,\n",
        "}\n\n",
        "func generated_try_step(v: int, fail: bool): Result<int, GeneratedStepError> {\n",
        "    if fail {\n",
        "        return Result.Err(GeneratedStepError.StepFailed)\n",
        "    }\n",
        "    return Result.Ok(v + 10)\n",
        "}\n\n",
        "func generated_try_pipeline(v: int, fail_first: bool, fail_second: bool): Result<int, GeneratedStepError> {\n",
        "    let a = generated_try_step(v, fail_first)?\n",
        "    let b = generated_try_step(a, fail_second)?\n",
        "    return Result.Ok(b + 5)\n",
        "}\n\n",
        "func generated_match_range(val: int): int {\n",
        "    return match val {\n",
        "        1 => 10\n",
        "        2..5 => 20\n",
        "        _ => 30\n",
        "    }\n",
        "}\n\n",
    );

    let source = replace_once(
        source,
        "struct Sample {",
        &format!("{declarations}struct Sample {{"),
    );

    let step_val = (base % 50).abs() + 1;
    let expected_pipeline_ok = step_val + 10 + 10 + 5;
    let scenario = format!(
        concat!(
            "    let pipe_ok_check = match generated_try_pipeline({step_val}, false, false) {{\n",
            "        Ok(v) => v == {expected_pipeline_ok}\n",
            "        Err(_) => false\n",
            "    }}\n",
            "    let pipe_fail1_check = match generated_try_pipeline({step_val}, true, false) {{\n",
            "        Ok(_) => false\n",
            "        Err(_) => true\n",
            "    }}\n",
            "    let pipe_fail2_check = match generated_try_pipeline({step_val}, false, true) {{\n",
            "        Ok(_) => false\n",
            "        Err(_) => true\n",
            "    }}\n",
            "    let coalesce_val: int? = nil\n",
            "    let coalesce_check = (coalesce_val ?? 42) == 42\n",
            "    let match_range_check = generated_match_range(1) == 10 && generated_match_range(3) == 20 && generated_match_range(9) == 30\n",
            "    if !pipe_ok_check || !pipe_fail1_check || !pipe_fail2_check || !coalesce_check || !match_range_check {{ return -1000057 }}\n",
        ),
        step_val = step_val,
        expected_pipeline_ok = expected_pipeline_ok,
    );

    replace_once(
        source,
        "    let base: int = ",
        &format!("{scenario}    let base: int = "),
    )
}

/// Add a seed-varying scenario exercising slice operations (len, isEmpty, first, last, subslice, splitAt)
/// and iterator adapters (iter.enumerate, iter.take, iter.skip, iter.stepBy, iter.count) with boundary and early-exit conditions.
fn add_seeded_slice_iter_case(source: String, seed: u64, base: i64) -> String {
    if seed % 16 != 8 {
        return source;
    }

    let declarations = concat!(
        "struct RangeIter {\n",
        "    current: int\n",
        "    end: int\n",
        "}\n\n",
        "func RangeIter.next(self: mut ref RangeIter): Option<int> {\n",
        "    if self.current < self.end {\n",
        "        let val = self.current\n",
        "        self.current = self.current + 1\n",
        "        return Option.Some(val)\n",
        "    }\n",
        "    return Option.None\n",
        "}\n\n",
        "func make_range_iter(start: int, end: int): RangeIter {\n",
        "    return RangeIter { current: start, end: end }\n",
        "}\n\n",
    );

    let source = replace_once(
        source,
        "import std.alloc.vec as vec",
        "import std.alloc.vec as vec\nimport std.core.iter as iter",
    );
    let source = replace_once(
        source,
        "struct Sample {",
        &format!("{declarations}struct Sample {{"),
    );

    let v0 = base + 10;
    let v1 = base + 20;
    let v2 = base + 30;
    let v3 = base + 40;
    let v4 = base + 50;
    let v5 = base + 60;
    let v6 = base + 70;
    let v7 = base + 80;

    let scenario = format!(
        concat!(
            // Phase 1: Vetores vazios (zero-capacity) e cortes no vazio
            "    let mut gen_empty_v = vec.Vec<int>.new()\n",
            "    let gen_empty_s = gen_empty_v.asSlice()\n",
            "    let gen_empty_len_ok = slice.len<int>(gen_empty_s) == (0 as usize) && slice.isEmpty<int>(gen_empty_s)\n",
            "    let gen_empty_first_ok = match slice.first<int>(gen_empty_s) {{\n",
            "        Some(_) => false\n",
            "        None => true\n",
            "    }}\n",
            "    let gen_empty_last_ok = match slice.last<int>(gen_empty_s) {{\n",
            "        Some(_) => false\n",
            "        None => true\n",
            "    }}\n",
            "    let gen_empty_get_ok = match slice.get<int>(gen_empty_s, 0 as usize) {{\n",
            "        Some(_) => false\n",
            "        None => true\n",
            "    }}\n",
            "    let gen_empty_sub_zero_ok = match slice.subslice<int>(gen_empty_s, 0 as usize, 0 as usize) {{\n",
            "        Some(sub) => slice.len<int>(sub) == (0 as usize) && slice.isEmpty<int>(sub)\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_empty_sub_oob1_ok = match slice.subslice<int>(gen_empty_s, 0 as usize, 1 as usize) {{\n",
            "        Some(_) => false\n",
            "        None => true\n",
            "    }}\n",
            "    let gen_empty_sub_oob2_ok = match slice.subslice<int>(gen_empty_s, 1 as usize, 0 as usize) {{\n",
            "        Some(_) => false\n",
            "        None => true\n",
            "    }}\n",
            "    let gen_empty_split_zero_ok = match slice.splitAt<int>(gen_empty_s, 0 as usize) {{\n",
            "        Some(sp) => slice.len<int>(sp.left) == (0 as usize) && slice.len<int>(sp.right) == (0 as usize)\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_empty_split_oob_ok = match slice.splitAt<int>(gen_empty_s, 1 as usize) {{\n",
            "        Some(_) => false\n",
            "        None => true\n",
            "    }}\n",
            "    if !gen_empty_len_ok {{ return -1000101 }}\n",
            "    if !gen_empty_first_ok {{ return -1000102 }}\n",
            "    if !gen_empty_last_ok {{ return -1000103 }}\n",
            "    if !gen_empty_get_ok {{ return -1000104 }}\n",
            "    if !gen_empty_sub_zero_ok {{ return -1000105 }}\n",
            "    if !gen_empty_sub_oob1_ok {{ return -1000106 }}\n",
            "    if !gen_empty_sub_oob2_ok {{ return -1000107 }}\n",
            "    if !gen_empty_split_zero_ok {{ return -1000108 }}\n",
            "    if !gen_empty_split_oob_ok {{ return -1000109 }}\n",

            // Phase 2: Capacidade pré-alocada (8), quase cheia (7) e cheia (8), cortes extremos
            "    let mut gen_cap_v = vec.Vec<int>.withCapacity(8 as usize)\n",
            "    gen_cap_v.push({v0})\n",
            "    gen_cap_v.push({v1})\n",
            "    gen_cap_v.push({v2})\n",
            "    gen_cap_v.push({v3})\n",
            "    gen_cap_v.push({v4})\n",
            "    gen_cap_v.push({v5})\n",
            "    gen_cap_v.push({v6})\n",
            "    let gen_almost_s = gen_cap_v.asSlice()\n",
            "    let gen_almost_ok = slice.len<int>(gen_almost_s) == (7 as usize) && gen_cap_v.capacity() >= (8 as usize)\n",
            "    gen_cap_v.push({v7})\n",
            "    let gen_full_s = gen_cap_v.asSlice()\n",
            "    let gen_full_ok = slice.len<int>(gen_full_s) == (8 as usize)\n",
            "    let gen_cut_empty_start = match slice.subslice<int>(gen_full_s, 0 as usize, 0 as usize) {{\n",
            "        Some(sub) => slice.len<int>(sub) == (0 as usize) && slice.isEmpty<int>(sub)\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_cut_empty_end = match slice.subslice<int>(gen_full_s, 8 as usize, 0 as usize) {{\n",
            "        Some(sub) => slice.len<int>(sub) == (0 as usize) && slice.isEmpty<int>(sub)\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_cut_head = match slice.subslice<int>(gen_full_s, 0 as usize, 3 as usize) {{\n",
            "        Some(sub) => slice.len<int>(sub) == (3 as usize) && sub[0] == {v0} && sub[1] == {v1} && sub[2] == {v2}\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_cut_tail = match slice.subslice<int>(gen_full_s, 5 as usize, 3 as usize) {{\n",
            "        Some(sub) => slice.len<int>(sub) == (3 as usize) && sub[0] == {v5} && sub[1] == {v6} && sub[2] == {v7}\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_cut_extending = match slice.subslice<int>(gen_full_s, 6 as usize, 3 as usize) {{\n",
            "        Some(_) => false\n",
            "        None => true\n",
            "    }}\n",
            "    let gen_cut_oob_start = match slice.subslice<int>(gen_full_s, 9 as usize, 0 as usize) {{\n",
            "        Some(_) => false\n",
            "        None => true\n",
            "    }}\n",
            "    if !gen_almost_ok {{ return -1000110 }}\n",
            "    if !gen_full_ok {{ return -1000111 }}\n",
            "    if !gen_cut_empty_start {{ return -1000112 }}\n",
            "    if !gen_cut_empty_end {{ return -1000113 }}\n",
            "    if !gen_cut_head {{ return -1000114 }}\n",
            "    if !gen_cut_tail {{ return -1000115 }}\n",
            "    if !gen_cut_extending {{ return -1000116 }}\n",
            "    if !gen_cut_oob_start {{ return -1000117 }}\n",

            // Phase 3: splitAt disjoint windows & múltiplas partes simultâneas
            "    let gen_split_0 = match slice.splitAt<int>(gen_full_s, 0 as usize) {{\n",
            "        Some(sp) => slice.len<int>(sp.left) == (0 as usize) && slice.len<int>(sp.right) == (8 as usize) && sp.right[0] == {v0} && sp.right[7] == {v7}\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_split_8 = match slice.splitAt<int>(gen_full_s, 8 as usize) {{\n",
            "        Some(sp) => slice.len<int>(sp.left) == (8 as usize) && slice.len<int>(sp.right) == (0 as usize) && sp.left[0] == {v0} && sp.left[7] == {v7}\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_split_oob = match slice.splitAt<int>(gen_full_s, 9 as usize) {{\n",
            "        Some(_) => false\n",
            "        None => true\n",
            "    }}\n",
            "    let gen_split_mid = match slice.splitAt<int>(gen_full_s, 4 as usize) {{\n",
            "        Some(sp) => slice.len<int>(sp.left) == (4 as usize) && slice.len<int>(sp.right) == (4 as usize) && (sp.left[1] + sp.right[2]) == ({v1} + {v6}) && (sp.left[3] + sp.right[0]) == ({v3} + {v4})\n",
            "        None => false\n",
            "    }}\n",
            "    if !gen_split_0 {{ return -1000118 }}\n",
            "    if !gen_split_8 {{ return -1000119 }}\n",
            "    if !gen_split_oob {{ return -1000120 }}\n",
            "    if !gen_split_mid {{ return -1000121 }}\n",

            // Phase 4: Ciclo de vida e mutação pós-corte (pop 8o, novo corte, destruição)
            "    let gen_popped = gen_cap_v.pop()\n",
            "    let gen_pop_ok = match gen_popped {{\n",
            "        Some(val) => val == {v7}\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_re_s = gen_cap_v.asSlice()\n",
            "    let gen_re_len_ok = slice.len<int>(gen_re_s) == (7 as usize)\n",
            "    let gen_re_last_ok = match slice.last<int>(gen_re_s) {{\n",
            "        Some(val) => *val == {v6}\n",
            "        None => false\n",
            "    }}\n",
            "    if !gen_pop_ok {{ return -1000122 }}\n",
            "    if !gen_re_len_ok {{ return -1000123 }}\n",
            "    if !gen_re_last_ok {{ return -1000124 }}\n",

            // Phase 5: Cadeias de iteradores e transferência de ownership
            "    let gen_chain_take = iter.take(make_range_iter({v0}, {v0} + 10), 8 as uint)\n",
            "    let gen_chain_skip = iter.skip(gen_chain_take, 2 as uint)\n",
            "    let gen_chain_step = iter.stepBy(gen_chain_skip, 2 as uint)\n",
            "    let gen_iter_chain_count = iter.count(gen_chain_step)\n",
            "    let gen_chain_count_ok = gen_iter_chain_count == (3 as uint)\n",
            "    let gen_zero_step_it = iter.stepBy(make_range_iter({v0}, {v0} + 5), 0 as uint)\n",
            "    let gen_zero_step_count = iter.count(gen_zero_step_it)\n",
            "    let gen_zero_step_ok = gen_zero_step_count == (5 as uint)\n",
            "    let mut gen_enum_it = iter.enumerate(make_range_iter({v0}, {v0} + 3))\n",
            "    let gen_enum_0 = match gen_enum_it.next() {{\n",
            "        Some(item) => item.index == (0 as uint) && item.value == {v0}\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_enum_1 = match gen_enum_it.next() {{\n",
            "        Some(item) => item.index == (1 as uint) && item.value == ({v0} + 1)\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_enum_2 = match gen_enum_it.next() {{\n",
            "        Some(item) => item.index == (2 as uint) && item.value == ({v0} + 2)\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_enum_end = match gen_enum_it.next() {{\n",
            "        Some(_) => false\n",
            "        None => true\n",
            "    }}\n",
            "    if !gen_chain_count_ok {{ return -1000125 }}\n",
            "    if !gen_zero_step_ok {{ return -1000126 }}\n",
            "    if !gen_enum_0 {{ return -1000127 }}\n",
            "    if !gen_enum_1 {{ return -1000128 }}\n",
            "    if !gen_enum_2 {{ return -1000129 }}\n",
            "    if !gen_enum_end {{ return -1000130 }}\n",

            // Phase 6: Pipelines com término antecipado, exaustão e over-skipping
            "    let mut gen_break_it = iter.take(make_range_iter({v0}, {v0} + 10), 5 as uint)\n",
            "    let mut gen_break_sum: int = 0\n",
            "    let mut gen_break_steps: int = 0\n",
            "    while true {{\n",
            "        match gen_break_it.next() {{\n",
            "            Some(val) => {{\n",
            "                gen_break_sum = gen_break_sum + val\n",
            "                gen_break_steps = gen_break_steps + 1\n",
            "                if gen_break_steps == 2 {{\n",
            "                    break\n",
            "                }}\n",
            "            }}\n",
            "            None => {{\n",
            "                break\n",
            "            }}\n",
            "        }}\n",
            "    }}\n",
            "    let gen_early_break_ok = gen_break_steps == 2 && gen_break_sum == ({v0} + {v0} + 1)\n",
            "    let mut gen_depleted_it = iter.take(make_range_iter({v0}, {v0} + 2), 10 as uint)\n",
            "    let gen_depleted_0 = match gen_depleted_it.next() {{\n",
            "        Some(val) => val == {v0}\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_depleted_1 = match gen_depleted_it.next() {{\n",
            "        Some(val) => val == ({v0} + 1)\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_depleted_end = match gen_depleted_it.next() {{\n",
            "        Some(_) => false\n",
            "        None => true\n",
            "    }}\n",
            "    let mut gen_overskip_it = iter.skip(make_range_iter({v0}, {v0} + 2), 10 as uint)\n",
            "    let gen_overskip_ok = match gen_overskip_it.next() {{\n",
            "        Some(_) => false\n",
            "        None => true\n",
            "    }}\n",
            "    if !gen_early_break_ok {{ return -1000131 }}\n",
            "    if !gen_depleted_0 {{ return -1000132 }}\n",
            "    if !gen_depleted_1 {{ return -1000133 }}\n",
            "    if !gen_depleted_end {{ return -1000134 }}\n",
            "    if !gen_overskip_ok {{ return -1000135 }}\n",
        ),
        v0 = v0,
        v1 = v1,
        v2 = v2,
        v3 = v3,
        v4 = v4,
        v5 = v5,
        v6 = v6,
        v7 = v7,
    );

    replace_once(
        source,
        "    let base: int = ",
        &format!("{scenario}    let base: int = "),
    )
}

/// Add a seed-varying scenario exercising Option, Result, and Cmp (Ordering, min, max, clamp, and Ord interface dispatch).
fn add_seeded_option_result_cmp_case(source: String, seed: u64, base: i64) -> String {
    if seed % 16 != 4 {
        return source;
    }

    let declarations = concat!(
        "struct PriorityItem {\n",
        "    priority: int\n",
        "    payload: int\n",
        "}\n\n",
        "func PriorityItem.cmp(self: ref PriorityItem, other: ref PriorityItem): cmp.Ordering {\n",
        "    if self.priority < other.priority {\n",
        "        return cmp.Ordering.Less\n",
        "    }\n",
        "    if self.priority > other.priority {\n",
        "        return cmp.Ordering.Greater\n",
        "    }\n",
        "    return cmp.Ordering.Equal\n",
        "}\n\n",
    );

    let source = replace_once(
        source,
        "import std.core.result as core_result",
        "import std.core.result as core_result\nimport std.core.option as core_option\nimport std.core.cmp as cmp",
    );
    let source = replace_once(
        source,
        "struct Sample {",
        &format!("{declarations}struct Sample {{"),
    );

    let v0 = base + 11;
    let v1 = base + 22;
    let v2 = base + 33;

    let scenario = format!(
        concat!(
            // 1. Option checks
            "    let gen_some_val: Option<int> = Option.Some({v0})\n",
            "    let gen_none_val: Option<int> = Option.None\n",
            "    if !gen_some_val.isSome() || gen_some_val.isNone() {{ return -1000201 }}\n",
            "    if gen_none_val.isSome() || !gen_none_val.isNone() {{ return -1000202 }}\n",
            "    if gen_some_val.unwrapOr({v1}) != {v0} {{ return -1000203 }}\n",
            "    if gen_none_val.unwrapOr({v1}) != {v1} {{ return -1000204 }}\n",
            "    let gen_nested_opt: Option<Option<int>> = Option.Some(Option.Some({v2}))\n",
            "    let gen_nested_ok = match gen_nested_opt {{\n",
            "        Some(inner) => match inner {{\n",
            "            Some(v) => v == {v2}\n",
            "            None => false\n",
            "        }}\n",
            "        None => false\n",
            "    }}\n",
            "    if !gen_nested_ok {{ return -1000205 }}\n",
            "    let gen_inner_none: Option<int> = Option.None\n",
            "    let gen_nested_none: Option<Option<int>> = Option.Some(gen_inner_none)\n",
            "    let gen_nested_none_ok = match gen_nested_none {{\n",
            "        Some(inner) => match inner {{\n",
            "            Some(_) => false\n",
            "            None => true\n",
            "        }}\n",
            "        None => false\n",
            "    }}\n",
            "    if !gen_nested_none_ok {{ return -1000206 }}\n",

            // 2. Result checks
            "    let gen_ok_res: Result<int, int> = Result.Ok({v0})\n",
            "    let gen_err_res: Result<int, int> = Result.Err({v1})\n",
            "    if !gen_ok_res.isOk() || gen_ok_res.isErr() {{ return -1000207 }}\n",
            "    if gen_err_res.isOk() || !gen_err_res.isErr() {{ return -1000208 }}\n",
            "    if gen_ok_res.unwrapOr({v2}) != {v0} {{ return -1000209 }}\n",
            "    if gen_err_res.unwrapOr({v2}) != {v2} {{ return -1000210 }}\n",
            "    let gen_ok_to_opt = gen_ok_res.ok()\n",
            "    if !gen_ok_to_opt.isSome() || gen_ok_to_opt.unwrapOr(0) != {v0} {{ return -1000211 }}\n",
            "    let gen_err_to_opt = gen_err_res.ok()\n",
            "    if gen_err_to_opt.isSome() || !gen_err_to_opt.isNone() {{ return -1000212 }}\n",
            "    let gen_ok_to_err = gen_ok_res.err()\n",
            "    if gen_ok_to_err.isSome() || !gen_ok_to_err.isNone() {{ return -1000213 }}\n",
            "    let gen_err_to_err = gen_err_res.err()\n",
            "    if !gen_err_to_err.isSome() || gen_err_to_err.unwrapOr(0) != {v1} {{ return -1000214 }}\n",

            // 3. Ordering enum & helpers
            "    if !cmp.Ordering.Less.isLess() || cmp.Ordering.Less.isEqual() || cmp.Ordering.Less.isGreater() {{ return -1000215 }}\n",
            "    if cmp.Ordering.Equal.isLess() || !cmp.Ordering.Equal.isEqual() || cmp.Ordering.Equal.isGreater() {{ return -1000216 }}\n",
            "    if cmp.Ordering.Greater.isLess() || cmp.Ordering.Greater.isEqual() || !cmp.Ordering.Greater.isGreater() {{ return -1000217 }}\n",
            "    if !cmp.Ordering.Equal.then(cmp.Ordering.Less).isLess() {{ return -1000218 }}\n",
            "    if !cmp.Ordering.Equal.then(cmp.Ordering.Greater).isGreater() {{ return -1000219 }}\n",
            "    if !cmp.Ordering.Less.then(cmp.Ordering.Greater).isLess() {{ return -1000220 }}\n",
            "    if !cmp.Ordering.Greater.then(cmp.Ordering.Less).isGreater() {{ return -1000221 }}\n",

            // 4. Scalar comparison helpers
            "    if cmp.min({v0}, {v1}) != {v0} {{ return -1000222 }}\n",
            "    if cmp.min({v1}, {v0}) != {v0} {{ return -1000223 }}\n",
            "    if cmp.max({v0}, {v1}) != {v1} {{ return -1000224 }}\n",
            "    if cmp.max({v1}, {v0}) != {v1} {{ return -1000225 }}\n",
            "    if cmp.clamp({v0} - 10, {v0}, {v1}) != {v0} {{ return -1000226 }}\n",
            "    if cmp.clamp({v0} + 5, {v0}, {v1}) != ({v0} + 5) {{ return -1000227 }}\n",
            "    if cmp.clamp({v1} + 10, {v0}, {v1}) != {v1} {{ return -1000228 }}\n",

            // 5. Generic Ord interface dispatch
            "    let gen_min_item = cmp.minBy(PriorityItem {{ priority: {v0}, payload: 100 }}, PriorityItem {{ priority: {v1}, payload: 200 }})\n",
            "    if gen_min_item.priority != {v0} || gen_min_item.payload != 100 {{ return -1000229 }}\n",
            "    let gen_max_item = cmp.maxBy(PriorityItem {{ priority: {v0}, payload: 100 }}, PriorityItem {{ priority: {v1}, payload: 200 }})\n",
            "    if gen_max_item.priority != {v1} || gen_max_item.payload != 200 {{ return -1000230 }}\n",
            "    let gen_clamp_low = cmp.clampBy(PriorityItem {{ priority: {v0} - 5, payload: 50 }}, PriorityItem {{ priority: {v0}, payload: 100 }}, PriorityItem {{ priority: {v1}, payload: 200 }})\n",
            "    if gen_clamp_low.priority != {v0} {{ return -1000231 }}\n",
            "    let gen_clamp_mid = cmp.clampBy(PriorityItem {{ priority: {v0} + 5, payload: 150 }}, PriorityItem {{ priority: {v0}, payload: 100 }}, PriorityItem {{ priority: {v1}, payload: 200 }})\n",
            "    if gen_clamp_mid.priority != ({v0} + 5) {{ return -1000232 }}\n",
            "    let gen_clamp_high = cmp.clampBy(PriorityItem {{ priority: {v1} + 5, payload: 250 }}, PriorityItem {{ priority: {v0}, payload: 100 }}, PriorityItem {{ priority: {v1}, payload: 200 }})\n",
            "    if gen_clamp_high.priority != {v1} {{ return -1000233 }}\n",
        ),
        v0 = v0,
        v1 = v1,
        v2 = v2,
    );

    replace_once(
        source,
        "    let base: int = ",
        &format!("{scenario}    let base: int = "),
    )
}

/// Add a seed-varying scenario exercising std.core.str (isEmpty, len, startsWith, endsWith, contains, find)
/// and std.core.mem (sizeOf, alignOf, swap, replace, take, refWrite, ptrOffset, ptrRead, ptrWrite).
fn add_seeded_str_mem_case(source: String, seed: u64, base: i64) -> String {
    if seed % 16 != 12 {
        return source;
    }

    let source = replace_once(
        source,
        "import std.core.result as core_result",
        "import std.core.result as core_result\nimport std.core.option as core_option\nimport std.core.str as core_str\nimport std.core.mem as mem",
    );

    let (full, full_len, prefix, suffix, mid_needle, mid_idx, non_matching) =
        if ((seed / 16) & 1) == 0 {
            ("arandu", 6, "aran", "ndu", "ran", 1, "rust")
        } else {
            ("compiler", 8, "comp", "ler", "pil", 3, "python")
        };

    let v0 = base + 41;
    let v1 = base + 73;
    let v2 = base + 97;

    let scenario = format!(
        concat!(
            // 1. String checks: empty, length, prefix, suffix, contains, find
            "    if !core_str.isEmpty(\"\") || core_str.isEmpty(\"{full}\") {{ return -1000301 }}\n",
            "    if core_str.len(\"\") != 0 as usize || core_str.len(\"{full}\") != {full_len} as usize || core_str.len(\"café\") != 5 as usize {{ return -1000302 }}\n",
            "    if !core_str.startsWith(\"{full}\", \"{prefix}\") || core_str.startsWith(\"{full}\", \"{non_matching}\") || !core_str.startsWith(\"{full}\", \"\") || !core_str.startsWith(\"\", \"\") || core_str.startsWith(\"\", \"{prefix}\") {{ return -1000303 }}\n",
            "    if !core_str.endsWith(\"{full}\", \"{suffix}\") || core_str.endsWith(\"{full}\", \"{prefix}\") || !core_str.endsWith(\"{full}\", \"\") || !core_str.endsWith(\"\", \"\") || core_str.endsWith(\"\", \"{suffix}\") {{ return -1000304 }}\n",
            "    if !core_str.contains(\"{full}\", \"{mid_needle}\") || core_str.contains(\"{full}\", \"{non_matching}\") || !core_str.contains(\"{full}\", \"\") || !core_str.contains(\"\", \"\") || core_str.contains(\"\", \"{mid_needle}\") {{ return -1000305 }}\n",
            "    let gen_find_prefix = core_str.find(\"{full}\", \"{prefix}\")\n",
            "    let gen_find_prefix_ok = match gen_find_prefix {{\n",
            "        Some(idx) => idx == 0 as usize\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_find_mid = core_str.find(\"{full}\", \"{mid_needle}\")\n",
            "    let gen_find_mid_ok = match gen_find_mid {{\n",
            "        Some(idx) => idx == {mid_idx} as usize\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_find_none = core_str.find(\"{full}\", \"{non_matching}\")\n",
            "    let gen_find_none_ok = match gen_find_none {{\n",
            "        Some(_) => false\n",
            "        None => true\n",
            "    }}\n",
            "    let gen_find_empty = core_str.find(\"{full}\", \"\")\n",
            "    let gen_find_empty_ok = match gen_find_empty {{\n",
            "        Some(idx) => idx == 0 as usize\n",
            "        None => false\n",
            "    }}\n",
            "    let gen_find_in_empty = core_str.find(\"\", \"a\")\n",
            "    let gen_find_in_empty_ok = match gen_find_in_empty {{\n",
            "        Some(_) => false\n",
            "        None => true\n",
            "    }}\n",
            "    let gen_find_empty_in_empty = core_str.find(\"\", \"\")\n",
            "    let gen_find_empty_in_empty_ok = match gen_find_empty_in_empty {{\n",
            "        Some(idx) => idx == 0 as usize\n",
            "        None => false\n",
            "    }}\n",
            "    if !gen_find_prefix_ok || !gen_find_mid_ok || !gen_find_none_ok || !gen_find_empty_ok || !gen_find_in_empty_ok || !gen_find_empty_in_empty_ok {{ return -1000306 }}\n",

            // 2. Memory intrinsics: sizeOf, alignOf
            "    if mem.sizeOf<int>() != 4 as usize || mem.alignOf<int>() != 4 as usize {{ return -1000307 }}\n",
            "    if mem.sizeOf<u8>() != 1 as usize || mem.alignOf<u8>() != 1 as usize {{ return -1000308 }}\n",
            "    if mem.sizeOf<u16>() != 2 as usize || mem.alignOf<u16>() != 2 as usize {{ return -1000309 }}\n",
            "    if mem.sizeOf<u64>() != 8 as usize || mem.alignOf<u64>() != 8 as usize {{ return -1000310 }}\n",
            "    if mem.sizeOf<usize>() < 4 as usize || mem.alignOf<usize>() < 4 as usize {{ return -1000311 }}\n",

            // 3. Memory value helpers: swap, replace, refWrite
            "    let mut gen_swap_a: int = {v0}\n",
            "    let mut gen_swap_b: int = {v1}\n",
            "    mem.swap<int>(mut ref gen_swap_a, mut ref gen_swap_b)\n",
            "    if gen_swap_a != {v1} || gen_swap_b != {v0} {{ return -1000312 }}\n",

            "    let mut gen_replace_dest: int = {v0}\n",
            "    let gen_old_val = mem.replace<int>(mut ref gen_replace_dest, {v2})\n",
            "    if gen_old_val != {v0} || gen_replace_dest != {v2} {{ return -1000313 }}\n",

            "    let mut gen_write_target: int = 0\n",
            "    mem.refWrite<int>(mut ref gen_write_target, {v0})\n",
            "    if gen_write_target != {v0} {{ return -1000314 }}\n",

            // 4. Raw pointer operations: ptrOffset, ptrRead, ptrWrite via heap slice
            "    let mut gen_mem_vec = vec.Vec<int>.new()\n",
            "    if !gen_mem_vec.tryPush({v0}) || !gen_mem_vec.tryPush({v1}) {{\n",
            "        return -1000315\n",
            "    }}\n",
            "    let gen_vec_slice = gen_mem_vec.asSlice()\n",
            "    let gen_raw_ptr = slice.asPtr<int>(gen_vec_slice)\n",
            "    let gen_read0 = unsafe {{ mem.ptrRead<int>(gen_raw_ptr) }}\n",
            "    let gen_ptr1 = unsafe {{ mem.ptrOffset<int>(gen_raw_ptr, 1 as isize) }}\n",
            "    let gen_read1 = unsafe {{ mem.ptrRead<int>(gen_ptr1) }}\n",
            "    unsafe {{ mem.ptrWrite<int>(gen_ptr1, {v2}) }}\n",
            "    let gen_read1_updated = unsafe {{ mem.ptrRead<int>(gen_ptr1) }}\n",
            "    let gen_vec_elem1 = match gen_mem_vec.getCopy(1 as usize) {{\n",
            "        Some(v) => v\n",
            "        None => 0\n",
            "    }}\n",
            "    if gen_read0 != {v0} || gen_read1 != {v1} || gen_read1_updated != {v2} || gen_vec_elem1 != {v2} {{ return -1000316 }}\n",
        ),
        full = full,
        full_len = full_len,
        prefix = prefix,
        suffix = suffix,
        mid_needle = mid_needle,
        mid_idx = mid_idx,
        non_matching = non_matching,
        v0 = v0,
        v1 = v1,
        v2 = v2,
    );

    replace_once(
        source,
        "    let base: int = ",
        &format!("{scenario}    let base: int = "),
    )
}

fn gen_expr(rng: &mut Rng, expected: Ty, depth: u8, base: i64) -> Expr {
    match expected {
        Ty::Int => gen_int(rng, depth, base),
        Ty::UInt => gen_uint(rng, depth, base as u64),
        Ty::USize => gen_usize(rng, depth, base as u64),
        Ty::ISize => gen_isize(rng, depth, base),
        Ty::Bool => gen_bool(rng, depth, base),
        Ty::Float => gen_float(rng, depth, base as f64),
        Ty::Char => gen_char(rng),
    }
}

fn gen_usize(rng: &mut Rng, depth: u8, base: u64) -> Expr {
    if depth == 0 {
        let value = base % 100;
        return Expr {
            ty: Ty::USize,
            source: format!("{value} as usize"),
            value: ConstantValue::USize(value),
        };
    }
    match rng.below(4) {
        0 => {
            let value = rng.below(MAX_BASE + 1);
            Expr {
                ty: Ty::USize,
                source: format!("{value} as usize"),
                value: ConstantValue::USize(value),
            }
        }
        1 => {
            let left = gen_usize(rng, depth - 1, base);
            let right = gen_usize(rng, depth - 1, base);
            let value = left.usize_value().wrapping_add(right.usize_value());
            Expr {
                ty: Ty::USize,
                source: format!("({} + {})", left.source, right.source),
                value: ConstantValue::USize(value),
            }
        }
        2 => {
            let left = gen_usize(rng, depth - 1, base);
            let right = gen_usize(rng, depth - 1, base);
            let left_v = left.usize_value();
            let right_v = right.usize_value();
            if left_v >= right_v {
                Expr {
                    ty: Ty::USize,
                    source: format!("({} - {})", left.source, right.source),
                    value: ConstantValue::USize(left_v - right_v),
                }
            } else {
                Expr {
                    ty: Ty::USize,
                    source: format!("({} + {})", left.source, right.source),
                    value: ConstantValue::USize(left_v.wrapping_add(right_v)),
                }
            }
        }
        _ => {
            let inner = gen_usize(rng, depth - 1, base);
            let value = inner.usize_value();
            Expr {
                ty: Ty::USize,
                source: format!("identity<usize>({})", inner.source),
                value: ConstantValue::USize(value),
            }
        }
    }
}

fn gen_isize(rng: &mut Rng, depth: u8, base: i64) -> Expr {
    if depth == 0 {
        let value = base % 50;
        return Expr {
            ty: Ty::ISize,
            source: format!("{value} as isize"),
            value: ConstantValue::ISize(value),
        };
    }
    match rng.below(3) {
        0 => {
            let value = rng.below(MAX_BASE + 1) as i64;
            Expr {
                ty: Ty::ISize,
                source: format!("{value} as isize"),
                value: ConstantValue::ISize(value),
            }
        }
        1 => {
            let left = gen_isize(rng, depth - 1, base);
            let right = gen_isize(rng, depth - 1, base);
            let value = left.isize_value().wrapping_add(right.isize_value());
            Expr {
                ty: Ty::ISize,
                source: format!("({} + {})", left.source, right.source),
                value: ConstantValue::ISize(value),
            }
        }
        _ => {
            let inner = gen_isize(rng, depth - 1, base);
            let value = inner.isize_value();
            Expr {
                ty: Ty::ISize,
                source: format!("identity<isize>({})", inner.source),
                value: ConstantValue::ISize(value),
            }
        }
    }
}

pub(crate) fn gen_synthesized_prefix(rng: &mut Rng) -> SynthesizedPrefix {
    let base = rng.below(MAX_BASE + 1);
    let base_value = i64::try_from(base).expect("base generator is bounded to small integers");
    let left = gen_expr(rng, Ty::Int, MAX_DEPTH, base_value);
    let unsigned = gen_expr(rng, Ty::UInt, MAX_DEPTH, base_value);
    let condition = gen_expr(rng, Ty::Bool, MAX_DEPTH, base_value);
    SynthesizedPrefix {
        base,
        base_value,
        left,
        unsigned,
        condition,
    }
}

fn gen_char(rng: &mut Rng) -> Expr {
    // Vary UTF-8 widths and include escaped controls/punctuation to exercise
    // scalar decoding. Bidi controls and surrogate code points stay excluded.
    const CHARS: [char; 14] = [
        'a', 'Z', '0', '_', 'é', 'λ', '中', '🦀', '\n', '\t', '\0', '\'', '\\', '"',
    ];
    let value = CHARS[rng.below(CHARS.len() as u64) as usize];
    Expr {
        ty: Ty::Char,
        source: char_literal(value),
        value: ConstantValue::Char(value),
    }
}

fn replace_once(source: String, expected: &str, replacement: &str) -> String {
    let Some((prefix, suffix)) = source.split_once(expected) else {
        panic!("synthesized source is missing expected template fragment: {expected:?}");
    };
    assert!(
        !suffix.contains(expected),
        "synthesized template fragment is ambiguous: {expected:?}"
    );
    format!("{prefix}{replacement}{suffix}")
}

/// Generate string expressions without allocating per fuzz iteration. The JIT
/// currently gives interpolated strings process lifetime, so putting one in the
/// long-running libFuzzer path would accumulate leaked buffers across inputs.
fn gen_string_expr(rng: &mut Rng) -> (&'static str, &'static str) {
    match rng.below(3) {
        0 => ("env.arg(1)", ""),
        1 => ("env.arg(2)", "argument-two"),
        _ => ("\"argument-two\"", "argument-two"),
    }
}

/// Generate unsigned expressions without underflow or zero division.
/// Leaves are at most `MAX_BASE`; each recursive level can at most double the
/// bound, so the current depth limit keeps every value below 2,000.
fn gen_uint(rng: &mut Rng, depth: u8, base: u64) -> Expr {
    if depth == 0 || rng.below(4) == 0 {
        let (source, value) = if rng.below(2) == 0 {
            ("uint_base".to_owned(), base)
        } else {
            let value = rng.below(MAX_BASE + 1);
            (format!("{value} as uint"), value)
        };
        return Expr {
            ty: Ty::UInt,
            source,
            value: ConstantValue::UInt(value),
        };
    }

    let left = gen_uint(rng, depth - 1, base);
    let left_value = left.uint_value();
    let (operator, right, value) = match rng.below(4) {
        0 => {
            let right = gen_uint(rng, depth - 1, base);
            let value = left_value + right.uint_value();
            ("+", right, value)
        }
        1 => {
            let right = Expr {
                ty: Ty::UInt,
                source: "2 as uint".to_owned(),
                value: ConstantValue::UInt(2),
            };
            ("*", right, left_value * 2)
        }
        2 => {
            let right = Expr {
                ty: Ty::UInt,
                source: "2 as uint".to_owned(),
                value: ConstantValue::UInt(2),
            };
            ("/", right, left_value / 2)
        }
        _ => {
            let right = Expr {
                ty: Ty::UInt,
                source: "2 as uint".to_owned(),
                value: ConstantValue::UInt(2),
            };
            ("%", right, left_value % 2)
        }
    };
    debug_assert_eq!(left.ty, Ty::UInt);
    debug_assert_eq!(right.ty, Ty::UInt);
    Expr {
        ty: Ty::UInt,
        source: format!("({} {operator} {})", left.source, right.source),
        value: ConstantValue::UInt(value),
    }
}

fn gen_float(rng: &mut Rng, depth: u8, base: f64) -> Expr {
    if depth == 0 || rng.below(4) == 0 {
        let (source, value) = if rng.below(2) == 0 {
            ("float_base".to_owned(), base)
        } else {
            let value = rng.below(MAX_BASE + 1) as f64;
            (float_literal(value), value)
        };
        return Expr {
            ty: Ty::Float,
            source,
            value: ConstantValue::Float(value),
        };
    }

    let left = gen_float(rng, depth - 1, base);
    let left_value = left.float_value();
    let (operator, right, value) = match rng.below(4) {
        0 => {
            let right = gen_float(rng, depth - 1, base);
            let value = left_value + right.float_value();
            ("+", right, value)
        }
        1 => {
            let right = gen_float(rng, depth - 1, base);
            let value = left_value - right.float_value();
            ("-", right, value)
        }
        2 => {
            let right = Expr {
                ty: Ty::Float,
                source: "2.0".to_owned(),
                value: ConstantValue::Float(2.0),
            };
            ("*", right, left_value * 2.0)
        }
        _ => {
            let right = Expr {
                ty: Ty::Float,
                source: "2.0".to_owned(),
                value: ConstantValue::Float(2.0),
            };
            ("/", right, left_value / 2.0)
        }
    };
    debug_assert_eq!(left.ty, Ty::Float);
    debug_assert_eq!(right.ty, Ty::Float);
    debug_assert!(value.is_finite());
    Expr {
        ty: Ty::Float,
        source: format!("({} {operator} {})", left.source, right.source),
        value: ConstantValue::Float(value),
    }
}

fn gen_int(rng: &mut Rng, depth: u8, base: i64) -> Expr {
    if depth == 0 || rng.below(4) == 0 {
        let (source, value) = if rng.below(2) == 0 {
            ("base".to_owned(), base)
        } else {
            let value = i64::try_from(rng.below(MAX_BASE + 1))
                .expect("literal generator is bounded to small integers");
            (value.to_string(), value)
        };
        return Expr {
            ty: Ty::Int,
            source,
            value: ConstantValue::Int(value),
        };
    }

    let left = gen_int(rng, depth - 1, base);
    let left_value = left.int_value();
    let operation = rng.below(5);
    let (operator, right, value) = match operation {
        0 => {
            let right = gen_int(rng, depth - 1, base);
            let value = left_value + right.int_value();
            ("+", right, value)
        }
        1 => {
            let right = gen_int(rng, depth - 1, base);
            let value = left_value - right.int_value();
            ("-", right, value)
        }
        2 => {
            let value = 1 + i64::try_from(rng.below(3)).expect("small multiplier");
            (
                "*",
                Expr {
                    ty: Ty::Int,
                    source: value.to_string(),
                    value: ConstantValue::Int(value),
                },
                left_value * value,
            )
        }
        3 => {
            let value = 1 + i64::try_from(rng.below(7)).expect("non-zero divisor");
            (
                "/",
                Expr {
                    ty: Ty::Int,
                    source: value.to_string(),
                    value: ConstantValue::Int(value),
                },
                left_value / value,
            )
        }
        _ => {
            let value = 1 + i64::try_from(rng.below(7)).expect("non-zero divisor");
            (
                "%",
                Expr {
                    ty: Ty::Int,
                    source: value.to_string(),
                    value: ConstantValue::Int(value),
                },
                left_value % value,
            )
        }
    };
    debug_assert_eq!(left.ty, Ty::Int);
    debug_assert_eq!(right.ty, Ty::Int);
    Expr {
        ty: Ty::Int,
        source: format!("({} {operator} {})", left.source, right.source),
        value: ConstantValue::Int(value),
    }
}

fn gen_bool(rng: &mut Rng, depth: u8, base: i64) -> Expr {
    if depth == 0 || rng.below(3) == 0 {
        let value = rng.below(2) == 0;
        return Expr {
            ty: Ty::Bool,
            source: if value { "true" } else { "false" }.to_owned(),
            value: ConstantValue::Bool(value),
        };
    }

    let operation = rng.below(16);
    if operation == 0 {
        let operand = gen_bool(rng, depth - 1, base);
        let value = !operand.bool_value();
        return Expr {
            ty: Ty::Bool,
            source: format!("!({})", operand.source),
            value: ConstantValue::Bool(value),
        };
    }

    if depth >= 3 && matches!(operation, 1 | 2) {
        let left = gen_bool(rng, depth - 2, base);
        let right = gen_bool(rng, depth - 2, base);
        let (operator, value) = if operation == 1 {
            ("&&", left.bool_value() && right.bool_value())
        } else {
            ("||", left.bool_value() || right.bool_value())
        };
        return Expr {
            ty: Ty::Bool,
            source: format!("({} {operator} {})", left.source, right.source),
            value: ConstantValue::Bool(value),
        };
    }

    if operation >= 12 {
        let left = gen_char(rng);
        let right = gen_char(rng);
        let operator = match rng.below(6) {
            0 => "==",
            1 => "!=",
            2 => "<",
            3 => "<=",
            4 => ">",
            _ => ">=",
        };
        let value = compare(left.char_value(), right.char_value(), operator);
        return Expr {
            ty: Ty::Bool,
            source: format!("({} {operator} {})", left.source, right.source),
            value: ConstantValue::Bool(value),
        };
    }

    let (left, right) = match operation % 3 {
        0 => (gen_int(rng, depth - 1, base), gen_int(rng, depth - 1, base)),
        1 => (
            gen_uint(rng, depth - 1, base as u64),
            gen_uint(rng, depth - 1, base as u64),
        ),
        _ => (
            gen_float(rng, depth - 1, base as f64),
            gen_float(rng, depth - 1, base as f64),
        ),
    };
    let operator = match rng.below(6) {
        0 => "==",
        1 => "!=",
        2 => "<",
        3 => "<=",
        4 => ">",
        _ => ">=",
    };
    let value = match (left.value, right.value) {
        (ConstantValue::Int(left), ConstantValue::Int(right)) => compare(left, right, operator),
        (ConstantValue::UInt(left), ConstantValue::UInt(right)) => compare(left, right, operator),
        (ConstantValue::Float(left), ConstantValue::Float(right)) => compare(left, right, operator),
        (ConstantValue::Char(left), ConstantValue::Char(right)) => compare(left, right, operator),
        (ConstantValue::Char(_), _) | (_, ConstantValue::Char(_)) => {
            unreachable!("typed comparison generator does not emit chars")
        }
        _ => unreachable!("typed comparison operands must have the same scalar type"),
    };
    debug_assert_eq!(left.ty, right.ty);
    Expr {
        ty: Ty::Bool,
        source: format!("({} {operator} {})", left.source, right.source),
        value: ConstantValue::Bool(value),
    }
}

fn compare<T: PartialOrd>(left: T, right: T, operator: &str) -> bool {
    match operator {
        "==" => left == right,
        "!=" => left != right,
        "<" => left < right,
        "<=" => left <= right,
        ">" => left > right,
        ">=" => left >= right,
        _ => unreachable!("comparison operator must come from the generated set"),
    }
}

/// Synthesizes a program containing a tree of nested `comptime if` statements.
///
/// Returns the full program source and the expected integer result determined by
/// the statically selected branch. Unselected branches contain syntactically valid
/// but semantically unavailable symbols to ensure discarded code is never resolved
/// or typed.
pub fn synthesize_nested_comptime_if(seed: u64, max_depth: usize) -> SynthesizedProgram {
    let mut rng = Rng::new(seed);
    let depth = max_depth.clamp(1, 4);
    let (body, expected_result) = gen_comptime_if_node(&mut rng, 0, depth);
    let source = format!("func main(): int {{\n{body}\n}}\n");
    SynthesizedProgram {
        source,
        expected_result,
    }
}

fn gen_comptime_if_node(rng: &mut Rng, current_depth: usize, max_depth: usize) -> (String, i32) {
    let select_then = rng.below(2) == 0;
    let condition_expr = if select_then {
        match rng.below(3) {
            0 => "true".to_string(),
            1 => {
                let a = i64::try_from(rng.below(50) + 1).expect("bounded static condition");
                format!("({a} + {a}) == {}", a * 2)
            }
            _ => "!false && ((3 * 3) == 9)".to_string(),
        }
    } else {
        match rng.below(3) {
            0 => "false".to_string(),
            1 => {
                let a = i64::try_from(rng.below(50) + 1).expect("bounded static condition");
                format!("({a} + {a}) == {}", a * 2 + 1)
            }
            _ => "!true || ((2 * 2) == 5)".to_string(),
        }
    };

    let indent = "    ".repeat(current_depth + 1);
    let next_depth = current_depth + 1;

    let (selected_body, expected_val) = if next_depth >= max_depth {
        let val = i32::try_from(rng.below(100) + 1).expect("bounded static result");
        (format!("{indent}return {val}"), val)
    } else {
        gen_comptime_if_node(rng, next_depth, max_depth)
    };

    let discarded_id = rng.below(10_000);
    let discarded_body = match rng.below(3) {
        0 => format!("{indent}return missing_callee_{discarded_id}()"),
        1 => format!("{indent}return nonexistent_symbol_{discarded_id}"),
        _ => format!(
            "{indent}comptime if false {{\n{indent}    return absent_inner_{discarded_id}()\n{indent}}} else {{\n{indent}    return missing_fallback_{discarded_id}()\n{indent}}}"
        ),
    };

    let (then_code, else_code) = if select_then {
        (selected_body, discarded_body)
    } else {
        (discarded_body, selected_body)
    };

    let body = format!(
        "{indent}comptime if {condition_expr} {{\n{then_code}\n{indent}}} else {{\n{else_code}\n{indent}}}"
    );

    (body, expected_val)
}
