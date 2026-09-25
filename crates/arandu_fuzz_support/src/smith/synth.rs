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
            ConstantValue::Bool(value) => value.to_string(),
            ConstantValue::Float(value) => float_literal(value),
            ConstantValue::Char(value) => char_literal(value),
        }
    }

    fn int_value(&self) -> i64 {
        match self.value {
            ConstantValue::Int(value) => value,
            ConstantValue::UInt(_)
            | ConstantValue::Bool(_)
            | ConstantValue::Float(_)
            | ConstantValue::Char(_) => {
                unreachable!("integer expression has non-integer value")
            }
        }
    }

    fn uint_value(&self) -> u64 {
        match self.value {
            ConstantValue::UInt(value) => value,
            ConstantValue::Int(_)
            | ConstantValue::Bool(_)
            | ConstantValue::Float(_)
            | ConstantValue::Char(_) => {
                unreachable!("unsigned expression has non-unsigned value")
            }
        }
    }

    fn bool_value(&self) -> bool {
        match self.value {
            ConstantValue::Bool(value) => value,
            ConstantValue::Int(_)
            | ConstantValue::UInt(_)
            | ConstantValue::Float(_)
            | ConstantValue::Char(_) => {
                unreachable!("boolean expression has non-boolean value")
            }
        }
    }

    fn float_value(&self) -> f64 {
        match self.value {
            ConstantValue::Float(value) => value,
            ConstantValue::Int(_)
            | ConstantValue::UInt(_)
            | ConstantValue::Bool(_)
            | ConstantValue::Char(_) => {
                unreachable!("float expression has non-float value")
            }
        }
    }

    fn char_value(&self) -> char {
        match self.value {
            ConstantValue::Char(value) => value,
            ConstantValue::Int(_)
            | ConstantValue::UInt(_)
            | ConstantValue::Bool(_)
            | ConstantValue::Float(_) => unreachable!("char expression has non-char value"),
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
    let (string_expr, expected_string) = gen_string_expr(&mut rng);
    let triple_candidates = [&left, &unsigned, &condition, &float_expr, &char_expr];
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
        "import io\nimport std.alloc.vec as vec\nimport std.core.slice as slice\nimport std.core.result as core_result\n\nstruct Sample {{\n    left: int\n    right: int\n    enabled: bool\n}}\n\nenum Choice {{\n    Left(int),\n    Right(int),\n    Flag(bool),\n}}\n\nfunc identity<T>(value: T): T {{\n    return value\n}}\n\nfunc read_ref(value: ref int): int {{\n    return *value\n}}\n\nfunc read_exclusive(value: mut ref int): int {{\n    return *value\n}}\n\nfunc adjust(value: int, enabled: bool): int {{\n    if enabled {{ return value + 2 }}\n    return value - 2\n}}\n\nfunc main(): int {{\n    let base: int = {base}\n    let float_base: float = {base}.0\n    let float_result: float = identity<float>({float_source})\n    if float_result != {float_expected} {{ return -1000000 }}\n    let values = [identity<int>({}), identity<int>({}), base]\n    let index: int = base % 3\n    {loop_source}\n    let sample = Sample {{ left: values[index], right: values[(index + 1) % 3] + total, enabled: identity<bool>({}) }}\n    let mut borrow_target: int = sample.left\n    let shared_value = read_ref(ref borrow_target)\n    let exclusive_value = read_exclusive(mut ref borrow_target)\n    let mut dynamic = vec.new<int>()\n    if !vec.tryReserve<int>(dynamic, 1 as uint) {{\n        vec.destroy<int>(dynamic)\n        return -1000001\n    }}\n    if vec.capacity<int>(dynamic) < 1 as uint || !vec.isEmpty<int>(dynamic) {{\n        vec.destroy<int>(dynamic)\n        return -1000002\n    }}\n    let mut dynamic_count: int = 0\n    while dynamic_count < 9 {{\n        if !vec.tryPush<int>(dynamic, sample.left + dynamic_count) {{\n            vec.destroy<int>(dynamic)\n            return -1000003\n        }}\n        dynamic_count = dynamic_count + 1\n    }}\n    let dynamic_len = vec.len<int>(dynamic) as int\n    let dynamic_view_len = slice.len<int>(vec.asSlice<int>(dynamic)) as int\n    if dynamic_view_len != dynamic_len {{\n        vec.destroy<int>(dynamic)\n        return -1000004\n    }}\n    let out_of_bounds = vec.get<int>(dynamic, dynamic_len as uint)\n    let out_of_bounds_has_value = match out_of_bounds {{\n        Some(_) => true\n        None => false\n    }}\n    if out_of_bounds_has_value {{\n        vec.destroy<int>(dynamic)\n        return -1000005\n    }}\n    if vec.put<int>(dynamic, dynamic_len as uint, -1) {{\n        vec.destroy<int>(dynamic)\n        return -1000006\n    }}\n    if dynamic_len != 9 {{\n        vec.destroy<int>(dynamic)\n        return -1000007\n    }}\n    let dynamic_index = (base % dynamic_len) as uint\n    let dynamic_element = vec.get<int>(dynamic, dynamic_index)\n    let dynamic_value = match dynamic_element {{\n        Some(value) => value\n        None => 0\n    }}\n    let expected_dynamic_value = sample.left + dynamic_index as int\n    if dynamic_value != expected_dynamic_value {{\n        vec.destroy<int>(dynamic)\n        return -1000008\n    }}\n    let dynamic_last = vec.get<int>(dynamic, 8 as uint)\n    let dynamic_tail = match dynamic_last {{\n        Some(value) => value\n        None => 0\n    }}\n    if dynamic_tail != sample.left + 8 {{\n        vec.destroy<int>(dynamic)\n        return -1000009\n    }}\n    if !vec.put<int>(dynamic, 8 as uint, sample.right) {{\n        vec.destroy<int>(dynamic)\n        return -1000010\n    }}\n    let updated_last = vec.get<int>(dynamic, 8 as uint)\n    let updated_value = match updated_last {{\n        Some(value) => value\n        None => 0\n    }}\n    if updated_value != sample.right {{\n        vec.destroy<int>(dynamic)\n        return -1000011\n    }}\n    let popped = vec.pop<int>(dynamic)\n    let popped_value = match popped {{\n        Some(value) => value\n        None => 0\n    }}\n    if popped_value != sample.right || vec.len<int>(dynamic) != 8 as uint {{\n        vec.destroy<int>(dynamic)\n        return -1000012\n    }}\n    vec.clear<int>(dynamic)\n    let empty_pop = vec.pop<int>(dynamic)\n    let empty_pop_has_value = match empty_pop {{\n        Some(_) => true\n        None => false\n    }}\n    if empty_pop_has_value {{\n        vec.destroy<int>(dynamic)\n        return -1000013\n    }}\n    vec.destroy<int>(dynamic)\n    let adjusted = adjust(shared_value, sample.enabled) + adjust(exclusive_value, !sample.enabled) + dynamic_value\n    let selected = if base % 3 == 0 {{ Choice.Flag(sample.enabled) }} else if base % 3 == 1 {{ Choice.Left(adjusted) }} else {{ Choice.Right(sample.right) }}\n    let result = match selected {{\n        Choice.Left(value) => value\n        Choice.Right(value) => value\n        Choice.Flag(enabled) => if enabled {{ 1 }} else {{ 0 }}\n    }}\n    io.println(result.to_str())\n    return result\n}}\n",
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
            "    let mut items = vec.new<int>()\n",
            "    vec.push<int>(items, value)\n",
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
            "    if owned_pack.marker != sample.right || vec.len<int>(owned_pack.items) != 1 as uint { return -1000053 }\n",
            "    let packed_value = vec.get<int>(owned_pack.items, 0 as uint)\n",
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
            "    return vec.new<T>()\n",
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
        "let mut dynamic = vec.new<int>()",
        "let mut dynamic = transfer<vec.Vec<int>>(relay<vec.Vec<int>>(make_vec<int>()))",
    );
    let source = replace_once(
        source,
        "    vec.destroy<int>(dynamic)\n    let adjusted =",
        "    dynamic.destroy()\n    let adjusted =",
    );
    let source = replace_once(
        source,
        "    let adjusted =",
        concat!(
            "    let mut flags = transfer<vec.Vec<bool>>(make_vec<bool>())\n",
            "    if !vec.tryReserve<bool>(flags, 1 as uint) {\n",
            "        vec.destroy<bool>(flags)\n",
            "        return -1000016\n",
            "    }\n",
            "    if !vec.tryPush<bool>(flags, sample.enabled) {\n",
            "        vec.destroy<bool>(flags)\n",
            "        return -1000017\n",
            "    }\n",
            "    let flags_view_len = slice.len<bool>(vec.asSlice<bool>(flags))\n",
            "    let first_flag = vec.get<bool>(flags, 0 as uint)\n",
            "    let first_flag_value = match first_flag {\n",
            "        Some(value) => value\n",
            "        None => false\n",
            "    }\n",
            "    if flags_view_len != 1 as uint || first_flag_value != sample.enabled {\n",
            "        vec.destroy<bool>(flags)\n",
            "        return -1000018\n",
            "    }\n",
            "    flags.destroy()\n",
            "    let mut choices = transfer<vec.Vec<Choice>>(make_vec<Choice>())\n",
            "    if !vec.tryPush<Choice>(choices, Choice.Flag(sample.enabled)) {\n",
            "        choices.destroy()\n",
            "        return -1000019\n",
            "    }\n",
            "    let selected_choice = vec.pop<Choice>(choices)\n",
            "    let recovered_flag = match selected_choice {\n",
            "        Some(Choice.Flag(value)) => value\n",
            "        Some(_) => false\n",
            "        None => false\n",
            "    }\n",
            "    if recovered_flag != sample.enabled {\n",
            "        choices.destroy()\n",
            "        return -1000020\n",
            "    }\n",
            "    if !vec.tryPush<Choice>(choices, Choice.Letter(sample.character)) {\n",
            "        choices.destroy()\n",
            "        return -1000021\n",
            "    }\n",
            "    let selected_character = vec.pop<Choice>(choices)\n",
            "    let recovered_character = match selected_character {\n",
            "        Some(Choice.Letter(value)) => value\n",
            "        Some(_) => 'a'\n",
            "        None => 'a'\n",
            "    }\n",
            "    if recovered_character != sample.character {\n",
            "        choices.destroy()\n",
            "        return -1000022\n",
            "    }\n",
            "    choices.destroy()\n",
            "    let mut characters = transfer<vec.Vec<char>>(make_vec<char>())\n",
            "    if !vec.tryReserve<char>(characters, 1 as uint) {\n",
            "        characters.destroy()\n",
            "        return -1000023\n",
            "    }\n",
            "    if !vec.tryPush<char>(characters, sample.character) {\n",
            "        characters.destroy()\n",
            "        return -1000024\n",
            "    }\n",
            "    let characters_view_len = slice.len<char>(vec.asSlice<char>(characters))\n",
            "    let first_character = vec.get<char>(characters, 0 as uint)\n",
            "    let recovered_vector_character = match first_character {\n",
            "        Some(value) => value\n",
            "        None => 'a'\n",
            "    }\n",
            "    if characters_view_len != 1 as uint || recovered_vector_character != sample.character {\n",
            "        characters.destroy()\n",
            "        return -1000025\n",
            "    }\n",
            "    characters.destroy()\n",
            "    let mut floats = transfer<vec.Vec<float>>(make_vec<float>())\n",
            "    if !vec.tryReserve<float>(floats, 1 as uint) {\n",
            "        floats.destroy()\n",
            "        return -1000026\n",
            "    }\n",
            "    if !vec.tryPush<float>(floats, float_result) {\n",
            "        floats.destroy()\n",
            "        return -1000027\n",
            "    }\n",
            "    let first_float = vec.get<float>(floats, 0 as uint)\n",
            "    let recovered_vector_float = match first_float {\n",
            "        Some(value) => value\n",
            "        None => 0.0\n",
            "    }\n",
            "    if recovered_vector_float != float_result {\n",
            "        floats.destroy()\n",
            "        return -1000028\n",
            "    }\n",
            "    floats.destroy()\n",
            "    let mut unsigned_values = transfer<vec.Vec<uint>>(relay<vec.Vec<uint>>(make_vec<uint>()))\n",
            "    if !vec.tryReserve<uint>(unsigned_values, 1 as uint) {\n",
            "        unsigned_values.destroy()\n",
            "        return -1000045\n",
            "    }\n",
            "    if !vec.tryPush<uint>(unsigned_values, uint_result) {\n",
            "        unsigned_values.destroy()\n",
            "        return -1000046\n",
            "    }\n",
            "    let unsigned_view_len = slice.len<uint>(vec.asSlice<uint>(unsigned_values))\n",
            "    let first_unsigned = vec.get<uint>(unsigned_values, 0 as uint)\n",
            "    let recovered_unsigned = match first_unsigned {\n",
            "        Some(value) => value\n",
            "        None => 0 as uint\n",
            "    }\n",
            "    if unsigned_view_len != 1 as uint || recovered_unsigned != uint_result {\n",
            "        unsigned_values.destroy()\n",
            "        return -1000047\n",
            "    }\n",
            "    let popped_unsigned = vec.pop<uint>(unsigned_values)\n",
            "    let recovered_popped_unsigned = match popped_unsigned {\n",
            "        Some(value) => value\n",
            "        None => 0 as uint\n",
            "    }\n",
            "    if recovered_popped_unsigned != uint_result || vec.len<uint>(unsigned_values) != 0 as uint {\n",
            "        unsigned_values.destroy()\n",
            "        return -1000048\n",
            "    }\n",
            "    unsigned_values.destroy()\n",
            "    let mut result_values = transfer<vec.Vec<core_result.Result<int, bool>>>(relay<vec.Vec<core_result.Result<int, bool>>>(make_vec<core_result.Result<int, bool>>()))\n",
            "    if !vec.tryReserve<core_result.Result<int, bool>>(result_values, 2 as uint) {\n",
            "        result_values.destroy()\n",
            "        return -1000049\n",
            "    }\n",
            "    if !vec.tryPush<core_result.Result<int, bool>>(result_values, Result.Ok(sample.left)) {\n",
            "        result_values.destroy()\n",
            "        return -1000050\n",
            "    }\n",
            "    if !vec.tryPush<core_result.Result<int, bool>>(result_values, Result.Err(sample.enabled)) {\n",
            "        result_values.destroy()\n",
            "        return -1000051\n",
            "    }\n",
            "    let nested_view_len = slice.len<core_result.Result<int, bool>>(vec.asSlice<core_result.Result<int, bool>>(result_values))\n",
            "    let first_nested_result = vec.get<core_result.Result<int, bool>>(result_values, 0 as uint)\n",
            "    let first_nested_valid = match first_nested_result {\n",
            "        Some(value) => value.isOk() && value.unwrapOr(-1) == sample.left\n",
            "        None => false\n",
            "    }\n",
            "    let second_nested_result = vec.pop<core_result.Result<int, bool>>(result_values)\n",
            "    let second_nested_valid = match second_nested_result {\n",
            "        Some(value) => value.isErr() && value.unwrapOr(-1) == -1\n",
            "        None => false\n",
            "    }\n",
            "    if nested_view_len != 2 as uint || !first_nested_valid || !second_nested_valid || vec.len<core_result.Result<int, bool>>(result_values) != 1 as uint {\n",
            "        result_values.destroy()\n",
            "        return -1000052\n",
            "    }\n",
            "    result_values.destroy()\n",
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
    let keys = (0..9_i64)
        .map(|index| base * 8 + index * 8)
        .collect::<Vec<_>>();
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
        "    match hash_map.get<GeneratedKey, int>(table, key) {\n",
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
    scenario.push_str("    let mut generated_map = hash_map.new<GeneratedKey, int>()\n");
    scenario.push_str("    let map_hashes_collide = ((hash_map.hashKey<GeneratedKey>(ref generated_map_key0) as uint) & 7) == ((hash_map.hashKey<GeneratedKey>(ref generated_map_key5) as uint) & 7)\n");
    for (index, value) in values.iter().enumerate().take(6) {
        scenario.push_str(&format!(
            "    hash_map.insert<GeneratedKey, int>(generated_map, generated_map_key{index}, {})\n",
            value
        ));
    }
    scenario.push_str("    let map_initial_capacity_ok = hash_map.capacity<GeneratedKey, int>(generated_map) == 8 as uint\n");
    scenario.push_str("    let map_initial_len_ok = hash_map.len<GeneratedKey, int>(generated_map) == 6 as uint\n");
    scenario.push_str(&format!(
        "    let map_previous = hash_map.insert<GeneratedKey, int>(generated_map, generated_map_key2, {})\n",
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
    scenario.push_str("    let map_removed_head = hash_map.remove<GeneratedKey, int>(generated_map, ref generated_map_key0)\n");
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
    scenario.push_str("    let map_removed_middle = hash_map.remove<GeneratedKey, int>(generated_map, ref generated_map_key3)\n");
    scenario.push_str("    let map_middle_shift_ok = match map_removed_middle {\n        Some(value) => value == ");
    scenario.push_str(&values[3].to_string());
    scenario.push_str(
        " && generated_map_value_or_minus_one(ref generated_map, ref generated_map_key4) == ",
    );
    scenario.push_str(&values[4].to_string());
    scenario.push_str("\n        None => false\n    }\n");
    for (index, value) in values.iter().enumerate().take(9).skip(6) {
        scenario.push_str(&format!(
            "    hash_map.insert<GeneratedKey, int>(generated_map, generated_map_key{index}, {})\n",
            value
        ));
    }
    scenario.push_str("    let map_growth_ok = hash_map.capacity<GeneratedKey, int>(generated_map) == 16 as uint && hash_map.len<GeneratedKey, int>(generated_map) == 7 as uint\n");
    scenario.push_str(&format!(
        "    let map_tail_ok = hash_map.contains<GeneratedKey, int>(generated_map, ref generated_map_key8) && generated_map_value_or_minus_one(ref generated_map, ref generated_map_key8) == {}\n",
        values[8]
    ));
    scenario.push_str("    hash_map.clear<GeneratedKey, int>(generated_map)\n");
    scenario.push_str("    let map_clear_ok = hash_map.isEmpty<GeneratedKey, int>(generated_map) && generated_map_value_or_minus_one(ref generated_map, ref generated_map_key5) == -1 && hash_map.capacity<GeneratedKey, int>(generated_map) == 16 as uint\n");
    scenario.push_str(&format!(
        "    hash_map.insert<GeneratedKey, int>(generated_map, generated_map_key2, {})\n",
        values[2]
    ));
    scenario.push_str(&format!(
        "    let map_reuse_ok = generated_map_value_or_minus_one(ref generated_map, ref generated_map_key2) == {}\n",
        values[2]
    ));
    scenario.push_str("    generated_map.destroy()\n");
    scenario.push_str("    if !map_hashes_collide || !map_initial_capacity_ok || !map_initial_len_ok || !map_previous_ok || !map_replacement_ok || !map_head_shift_ok || !map_middle_shift_ok || !map_growth_ok || !map_tail_ok || !map_clear_ok || !map_reuse_ok { return -1000055 }\n");

    replace_once(
        source,
        "    let base: int = ",
        &format!("{scenario}    let base: int = "),
    )
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
            "    let mut generated_bits = bitset.bitsetNew()\n",
            "    let generated_bit0 = generated_bits.insert(0 as uint)\n",
            "    let generated_bit63 = generated_bits.insert(63 as uint)\n",
            "    let generated_bit64 = generated_bits.insert(64 as uint)\n",
            "    let generated_bit127 = generated_bits.insert(127 as uint)\n",
            "    let generated_bit_seed = generated_bits.insert({seed_bit} as uint)\n",
            "    let generated_bit_duplicate = generated_bits.insert(64 as uint)\n",
            "    let generated_bit_membership = generated_bits.contains(0 as uint) && generated_bits.contains(63 as uint) && generated_bits.contains(64 as uint) && generated_bits.contains(127 as uint) && generated_bits.contains({seed_bit} as uint)\n",
            "    let generated_bit_count = generated_bits.countOnes() == 5 as uint\n",
            "    let generated_bit_removed = generated_bits.remove(64 as uint)\n",
            "    let generated_bit_shift_safe = !generated_bits.contains(64 as uint) && generated_bits.contains(63 as uint) && generated_bits.contains(127 as uint) && generated_bits.countOnes() == 4 as uint\n",
            "    let generated_bit_missing_remove = generated_bits.remove(64 as uint)\n",
            "    generated_bits.clear()\n",
            "    let generated_bit_clear = generated_bits.countOnes() == 0 as uint && !generated_bits.contains(127 as uint)\n",
            "    let generated_bit_reuse = generated_bits.insert({seed_bit} as uint) && generated_bits.contains({seed_bit} as uint) && generated_bits.countOnes() == 1 as uint\n",
            "    if !generated_bit0 || !generated_bit63 || !generated_bit64 || !generated_bit127 || !generated_bit_seed || generated_bit_duplicate || !generated_bit_membership || !generated_bit_count || !generated_bit_removed || !generated_bit_shift_safe || generated_bit_missing_remove || !generated_bit_clear || !generated_bit_reuse {{ return -1000056 }}\n",
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

fn gen_expr(rng: &mut Rng, expected: Ty, depth: u8, base: i64) -> Expr {
    match expected {
        Ty::Int => gen_int(rng, depth, base),
        Ty::UInt => gen_uint(rng, depth, base as u64),
        Ty::Bool => gen_bool(rng, depth, base),
        Ty::Float => gen_float(rng, depth, base as f64),
        Ty::Char => gen_char(rng),
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
