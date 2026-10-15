//! Bounded, deterministic rejection oracle for mutations of valid programs.
//!
//! Each mutation changes one known construct and carries its expected diagnostic
//! code. This is separate from arbitrary-source recovery: accepting an invalid
//! mutation is a failure even when the compiler does not panic.

use arandu_middle::{DiagCode, Diagnostic, Severity};
use arandu_query::AnalysisHost;

use super::artifact::Failure;

pub(crate) mod structural;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mutation {
    GuardedWildcard,
    #[cfg(test)]
    GuardedBinding,
    MissingBrace,
    UnterminatedUnicodeString,
    RuntimeCapture,
    AssignmentType,
    ZeroDivisor,
}

impl Mutation {
    #[cfg(test)]
    const ALL: [Self; 7] = [
        Self::GuardedWildcard,
        Self::GuardedBinding,
        Self::MissingBrace,
        Self::UnterminatedUnicodeString,
        Self::RuntimeCapture,
        Self::AssignmentType,
        Self::ZeroDivisor,
    ];

    #[cfg(test)]
    fn case(self, seed: u64) -> Case {
        let value = seed % 127 + 1;
        let (valid, from, to, expected) = match self {
            Self::GuardedWildcard | Self::GuardedBinding => (
                format!("enum Color {{ Red, Blue }}\nfunc pick(c: Color): int {{ return match c {{ _ => {value} }} }}\n"),
                "_ =>",
                if self == Self::GuardedWildcard { "_ if false =>" } else { "value if false =>" },
                DiagCode::T024NonExhaustiveMatch,
            ),
            Self::MissingBrace => (
                format!("func main(): int {{ if true {{ return {value} }}; return 0 }}\n"),
                "; return 0 }",
                "; return 0",
                DiagCode::P001UnexpectedToken,
            ),
            Self::UnterminatedUnicodeString => (
                format!("func main(): void {{ let text = \"ação_界_🦀_{value}\" }}\n"),
                "\" }",
                " }",
                DiagCode::LX001UnterminatedString,
            ),
            Self::RuntimeCapture => (
                format!("func main(): int {{ let outside = {value}; return comptime ({value} + 1) }}\n"),
                "return comptime (",
                "return comptime (outside + ",
                DiagCode::T043ComptimeRuntimeCapture,
            ),
            Self::AssignmentType => (
                format!("func main(): int {{ let answer: int = {value}; return answer }}\n"),
                ": int = ",
                ": bool = ",
                DiagCode::T002IncompatibleAssignment,
            ),
            Self::ZeroDivisor => (
                format!("func main(): int {{ return {value} / (1) }}\n"),
                "/ (1)",
                "/ (0)",
                DiagCode::T040DivisionByZero,
            ),
        };
        let invalid = valid.replacen(from, to, 1);
        Case {
            mutation: self,
            valid,
            invalid,
            expected,
        }
    }
}

#[derive(Debug)]
struct Case {
    mutation: Mutation,
    valid: String,
    invalid: String,
    expected: DiagCode,
}

fn diagnostics(source: &str) -> Vec<Diagnostic> {
    let mut host = AnalysisHost::new();
    let file = host.new_file("negative/generated.aru".into(), source.to_owned());
    let syntax = arandu_query::passes::parse_diagnostics(host.db(), file);
    if syntax
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        return syntax.to_vec();
    }
    arandu_query::passes::type_check(host.db(), file)
        .diagnostics
        .to_vec()
}

#[cfg(test)]
fn verify(case: &Case) -> Result<(), Failure> {
    let baseline = diagnostics(&case.valid);
    if baseline
        .iter()
        .any(|d| d.severity == Severity::Error || d.is_ice())
    {
        return Err(Failure::new(
            "invalid-negative-baseline",
            format!("{baseline:?}"),
            false,
        ));
    }
    verify_rejection(case)
}

fn verify_rejection(case: &Case) -> Result<(), Failure> {
    let failure = |kind, message: String| {
        Failure::new(
            kind,
            format!(
                "{:?}: {message}\nbaseline:\n{}\nmutant:\n{}",
                case.mutation, case.valid, case.invalid
            ),
            false,
        )
    };
    let rejected = diagnostics(&case.invalid);
    if rejected.iter().any(Diagnostic::is_ice) {
        return Err(failure("negative-mutation-ice", format!("{rejected:?}")));
    }
    if !rejected.iter().any(|diagnostic| {
        diagnostic.code == case.expected && diagnostic.severity == Severity::Error
    }) {
        return Err(failure(
            "negative-mutation-diagnostic",
            format!("expected {:?}, received {rejected:?}", case.expected),
        ));
    }
    // Compare fresh analyses, including labels, hints and ordering. A repeated
    // memoized query would only compare an object with itself.
    if rejected != diagnostics(&case.invalid) {
        return Err(failure(
            "negative-mutation-nondeterminism",
            format!("first analysis: {rejected:?}"),
        ));
    }
    Ok(())
}

/// Run alongside arbitrary-source pipeline recovery in the existing fuzz target.
pub(crate) fn run(data: &[u8]) {
    structural::run(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_negative_mutation_rejects_with_its_structured_code() {
        for mutation in Mutation::ALL {
            for seed in [0, 1, u64::MAX] {
                let case = mutation.case(seed);
                assert_ne!(case.valid, case.invalid);
                verify(&case).unwrap_or_else(|failure| panic!("{failure:?}"));
            }
        }
    }

    #[test]
    fn pipeline_negative_mutations_accept_arbitrary_generator_bytes() {
        for selector in 0..7 {
            crate::run(crate::Target::Pipeline, &[selector]);
        }
    }
}
