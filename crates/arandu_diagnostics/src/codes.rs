use std::fmt;

/// | Prefix | Phase |
/// |--------|-------|
/// | `LX`   | Lexer |
/// | `P`    | Parser |
/// | `M`    | Module / import resolution |
/// | `N`    | Name resolution / scope |
/// | `T`    | Type checker |
/// | `L`    | Lowering |
/// | `G`    | Generics |
/// | `O`    | Ownership / move checker |
/// | `W`    | Warnings / linting |
/// | `U`    | Unimplemented / future features |
/// | `ICE`  | Internal compiler error |
///
/// **Single source of truth** for which codes exist: this enum (plus
/// [`DiagCode::as_str`] / [`DiagCode::ALL`]). Doc validation (xtask
/// `check-diag-docs`) compares `docs/errors/{code}.md` against `ALL` —
/// never a hand-maintained parallel list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagCode {
    // ── Lexical Analysis (LX) ──
    LX001UnterminatedString,
    LX002InvalidUnicodeChar,
    LX003InvalidNumericLiteral,
    /// Unescaped bidirectional Unicode control character (Trojan Source, CWE-1307).
    LX004BidiTrojanSource,

    // ── Parser / Syntax (P) ──
    P001UnexpectedToken,
    P002UnclosedBlock,
    P003InvalidAssignmentOperator,
    P004ExpectedIdentifier,
    P005ExpectedExpression,
    P006MalformedAttribute,

    // ── Modules & Imports (M) ──
    M001UnresolvedImport,
    M002UndefinedNamespaceMember,
    M003NamespaceUsedAsValue,
    M004LegacyLocalImport,
    M005FilesystemImportForbidden,

    // ── Name Resolution / Scope (N) ──
    N001UndefinedValue,
    N002UndefinedType,
    N003RedefinedName,
    N004TypeUsedAsValue,
    N005ValueUsedAsType,
    N006ImportConflict,
    N007UndefinedAssignmentTarget,
    N010UndefinedAssociatedFunction,
    N011BreakContinueOutsideLoop,
    N012UnknownAnnotation,
    N013InvalidAnnotationTarget,
    N014InvalidAnnotationArguments,
    N015DuplicateAnnotation,
    /// An internal declaration was imported from outside its package.
    N016InternalOutsidePackage,
    /// An implementation of a sealed interface is outside its package.
    N017SealedImplOutsidePackage,
    /// A re-export is more visible than its source declaration allows.
    N018ReExportNarrowing,
    /// A cycle in module re-exports prevents a stable public surface.
    N019CyclicReExport,

    // ── Type Checker (T) ──
    T001CannotInferType,
    T002IncompatibleAssignment,
    T003IncompatibleCallArg,
    T004IncompatibleReturnType,
    T005OperatorNotApplicable,
    T006NotNullable,
    T007IfBranchMismatch,
    T008MatchArmMismatch,
    T009ConditionNotBool,
    T010InvalidCast,
    T011GenericConstraintNotSatisfied,
    T012WrongArgCount,
    T013UnknownNamedArg,
    T014InvalidVariadicType,
    T015ImplicitWidening,
    T016TryInvalid,
    T017InvalidIndex,
    T018UndefinedField,
    T021MethodSelfRequired,
    T024NonExhaustiveMatch,
    T025InterfaceNotSatisfied,
    T026CannotAssignImmutable,
    T027MissingStructFields,
    T028DuplicateFieldInit,
    T029RecursiveStructInfiniteSize,
    T030DuplicateFieldDecl,
    T031Reserved,
    T032AwaitInvalid,
    T033IndirectCallNotSupported,
    /// Cannot auto-format a value as `str` (ToStr v0.1: only primitives).
    T034CannotFormat,
    /// Invalid signature or duplicate association for `@Destructor`.
    T035InvalidDestructor,
    /// Invalid signature for a function marked with `@Test`.
    T036InvalidTestContract,
    /// Invalid signature for a function marked with `@Benchmark`.
    T037InvalidBenchmarkContract,
    /// Integer literal cannot be represented by its contextual integer type.
    T038IntegerLiteralOutOfRange,
    /// Function performs an effect that is not declared in @Effects or is denied by policy.
    T039UnsatisfiedEffect,
    /// Attempt to divide or calculate remainder with zero divisor.
    T040DivisionByZero,
    /// Access to a private struct field outside its defining struct.
    T041PrivateFieldAccess,
    /// Type or staging construct outside the public scalar CTFE subset.
    T042UnsupportedComptime,
    /// A compile-time root reads or writes a runtime local/parameter.
    T043ComptimeRuntimeCapture,
    /// CTFE cannot admit or evaluate the retained operations/call closure.
    T044ComptimeEvaluationFailed,
    /// CTFE exhausted its fuel, frame or value budget.
    T045ComptimeLimitExceeded,
    /// Checked compile-time arithmetic overflow or invalid shift.
    T046ComptimeArithmetic,
    /// A requested type has no finite, concrete layout on the selected target.
    T047InvalidTypeLayout,
    /// Array repetition requires a static length and safely duplicable elements.
    T048InvalidArrayRepeat,

    // ── Lowering (L) ──
    L001LoweringUnresolvedSymbol,

    // ── Generics (G) ──
    G001GenericInstantiationCycle,
    G002GenericInstantiationLimit,

    // ── Ownership / Memory (O) ──
    // Roadmap F2/M2/G2: O002 move-while-borrowed · O003 exclusive conflict ·
    // O004 generational-fallback info · O006 destroy/free while borrowed.
    O001UseAfterMove,
    /// Move/consume while an active borrow exists (M2).
    O002MoveWhileBorrowed,
    /// Exclusive borrow conflict (`&mut` vs any) or mutation under shared borrow.
    O003MutableBorrowConflict,
    /// Informative: generational/escape fallback (G2 / F2.3) — not shared-borrow conflict.
    O004GenerationalFallback,
    O005DoubleFree,
    /// Destroy/free/drop while a borrow is still active (M2; static double-free).
    O006DestroyWhileBorrowed,
    O007InconsistentMoveBetweenBranches,
    O008UseBeforeInit,
    O009LifetimeMismatch,
    O010EscapeOfBorrowedValue,
    O011FreeRequiresPtr,
    O012AllocRequiresUnsafe,
    O013ExternRequiresUnsafe,
    O014FreeRequiresUnsafe,

    // ── Warnings & Linting (W) ──
    W001VariableAssignedNotUsed,
    W002DeadCode,
    W003UnreachableCode,
    W004VariableShadowing,
    W005UnnecessaryMutability,
    W006UnhandledResult,
    W007UnusedImport,
    W008LegacyAnnotationName,

    // ── Unimplemented (U) ──
    U001FeatureNotSupported,

    // ── Internal Compiler Errors (ICE) ──
    ICELX001,
    ICEP001,
    ICEN001,
    ICET001,
    ICEO001,
    ICEL001,
    ICEGEN001,
    ICEGEN002,
}

impl DiagCode {
    /// Every variant of this enum, in declaration order.
    ///
    /// Adding a new `DiagCode` without updating this array is a compile error
    /// if you also extend the exhaustiveness helper in tests — prefer updating
    /// `ALL` in the same edit as the new variant (and `as_str`).
    pub const ALL: &'static [DiagCode] = &{
        use DiagCode::*;
        [
            LX001UnterminatedString,
            LX002InvalidUnicodeChar,
            LX003InvalidNumericLiteral,
            LX004BidiTrojanSource,
            P001UnexpectedToken,
            P002UnclosedBlock,
            P003InvalidAssignmentOperator,
            P004ExpectedIdentifier,
            P005ExpectedExpression,
            P006MalformedAttribute,
            M001UnresolvedImport,
            M002UndefinedNamespaceMember,
            M003NamespaceUsedAsValue,
            M004LegacyLocalImport,
            M005FilesystemImportForbidden,
            N001UndefinedValue,
            N002UndefinedType,
            N003RedefinedName,
            N004TypeUsedAsValue,
            N005ValueUsedAsType,
            N006ImportConflict,
            N007UndefinedAssignmentTarget,
            N010UndefinedAssociatedFunction,
            N011BreakContinueOutsideLoop,
            N012UnknownAnnotation,
            N013InvalidAnnotationTarget,
            N014InvalidAnnotationArguments,
            N015DuplicateAnnotation,
            N016InternalOutsidePackage,
            N017SealedImplOutsidePackage,
            N018ReExportNarrowing,
            N019CyclicReExport,
            T001CannotInferType,
            T002IncompatibleAssignment,
            T003IncompatibleCallArg,
            T004IncompatibleReturnType,
            T005OperatorNotApplicable,
            T006NotNullable,
            T007IfBranchMismatch,
            T008MatchArmMismatch,
            T009ConditionNotBool,
            T010InvalidCast,
            T011GenericConstraintNotSatisfied,
            T012WrongArgCount,
            T013UnknownNamedArg,
            T014InvalidVariadicType,
            T015ImplicitWidening,
            T016TryInvalid,
            T017InvalidIndex,
            T018UndefinedField,
            T021MethodSelfRequired,
            T024NonExhaustiveMatch,
            T025InterfaceNotSatisfied,
            T026CannotAssignImmutable,
            T027MissingStructFields,
            T028DuplicateFieldInit,
            T029RecursiveStructInfiniteSize,
            T030DuplicateFieldDecl,
            T031Reserved,
            T032AwaitInvalid,
            T033IndirectCallNotSupported,
            T034CannotFormat,
            T035InvalidDestructor,
            T036InvalidTestContract,
            T037InvalidBenchmarkContract,
            T038IntegerLiteralOutOfRange,
            T039UnsatisfiedEffect,
            T040DivisionByZero,
            T041PrivateFieldAccess,
            T042UnsupportedComptime,
            T043ComptimeRuntimeCapture,
            T044ComptimeEvaluationFailed,
            T045ComptimeLimitExceeded,
            T046ComptimeArithmetic,
            T047InvalidTypeLayout,
            T048InvalidArrayRepeat,
            L001LoweringUnresolvedSymbol,
            G001GenericInstantiationCycle,
            G002GenericInstantiationLimit,
            O001UseAfterMove,
            O002MoveWhileBorrowed,
            O003MutableBorrowConflict,
            O004GenerationalFallback,
            O005DoubleFree,
            O006DestroyWhileBorrowed,
            O007InconsistentMoveBetweenBranches,
            O008UseBeforeInit,
            O009LifetimeMismatch,
            O010EscapeOfBorrowedValue,
            O011FreeRequiresPtr,
            O012AllocRequiresUnsafe,
            O013ExternRequiresUnsafe,
            O014FreeRequiresUnsafe,
            W001VariableAssignedNotUsed,
            W002DeadCode,
            W003UnreachableCode,
            W004VariableShadowing,
            W005UnnecessaryMutability,
            W006UnhandledResult,
            W007UnusedImport,
            W008LegacyAnnotationName,
            U001FeatureNotSupported,
            ICELX001,
            ICEP001,
            ICEN001,
            ICET001,
            ICEO001,
            ICEL001,
            ICEGEN001,
            ICEGEN002,
        ]
    };

    /// Returns `true` if this diagnostic code represents an Internal Compiler Error (ICE).
    #[must_use]
    pub fn is_ice(self) -> bool {
        matches!(
            self,
            DiagCode::ICELX001
                | DiagCode::ICEP001
                | DiagCode::ICEN001
                | DiagCode::ICET001
                | DiagCode::ICEO001
                | DiagCode::ICEL001
                | DiagCode::ICEGEN001
                | DiagCode::ICEGEN002
        )
    }

    /// User-facing codes that require `docs/errors/{as_str()}.md`.
    ///
    /// ICE codes are internal and are documented elsewhere (if at all).
    #[must_use]
    pub fn requires_error_doc(self) -> bool {
        !self.is_ice()
    }

    /// File stem for `docs/errors/{stem}.md` (same as [`Self::as_str`] for user codes).
    #[must_use]
    pub fn doc_stem(self) -> &'static str {
        self.as_str()
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DiagCode::LX001UnterminatedString => "LX001",
            DiagCode::LX002InvalidUnicodeChar => "LX002",
            DiagCode::LX003InvalidNumericLiteral => "LX003",
            DiagCode::LX004BidiTrojanSource => "LX004",
            DiagCode::P001UnexpectedToken => "P001",
            DiagCode::P002UnclosedBlock => "P002",
            DiagCode::P003InvalidAssignmentOperator => "P003",
            DiagCode::P004ExpectedIdentifier => "P004",
            DiagCode::P005ExpectedExpression => "P005",
            DiagCode::P006MalformedAttribute => "P006",
            DiagCode::M001UnresolvedImport => "M001",
            DiagCode::M002UndefinedNamespaceMember => "M002",
            DiagCode::M003NamespaceUsedAsValue => "M003",
            DiagCode::M004LegacyLocalImport => "M004",
            DiagCode::M005FilesystemImportForbidden => "M005",
            DiagCode::N001UndefinedValue => "N001",
            DiagCode::N002UndefinedType => "N002",
            DiagCode::N003RedefinedName => "N003",
            DiagCode::N004TypeUsedAsValue => "N004",
            DiagCode::N005ValueUsedAsType => "N005",
            DiagCode::N006ImportConflict => "N006",
            DiagCode::N007UndefinedAssignmentTarget => "N007",
            DiagCode::N010UndefinedAssociatedFunction => "N010",
            DiagCode::N011BreakContinueOutsideLoop => "N011",
            DiagCode::N012UnknownAnnotation => "N012",
            DiagCode::N013InvalidAnnotationTarget => "N013",
            DiagCode::N014InvalidAnnotationArguments => "N014",
            DiagCode::N015DuplicateAnnotation => "N015",
            DiagCode::N016InternalOutsidePackage => "N016",
            DiagCode::N017SealedImplOutsidePackage => "N017",
            DiagCode::N018ReExportNarrowing => "N018",
            DiagCode::N019CyclicReExport => "N019",
            DiagCode::T001CannotInferType => "T001",
            DiagCode::T002IncompatibleAssignment => "T002",
            DiagCode::T003IncompatibleCallArg => "T003",
            DiagCode::T004IncompatibleReturnType => "T004",
            DiagCode::T005OperatorNotApplicable => "T005",
            DiagCode::T006NotNullable => "T006",
            DiagCode::T007IfBranchMismatch => "T007",
            DiagCode::T008MatchArmMismatch => "T008",
            DiagCode::T009ConditionNotBool => "T009",
            DiagCode::T010InvalidCast => "T010",
            DiagCode::T011GenericConstraintNotSatisfied => "T011",
            DiagCode::T012WrongArgCount => "T012",
            DiagCode::T013UnknownNamedArg => "T013",
            DiagCode::T014InvalidVariadicType => "T014",
            DiagCode::T015ImplicitWidening => "T015",
            DiagCode::T016TryInvalid => "T016",
            DiagCode::T017InvalidIndex => "T017",
            DiagCode::T018UndefinedField => "T018",
            DiagCode::T021MethodSelfRequired => "T021",
            DiagCode::T024NonExhaustiveMatch => "T024",
            DiagCode::T025InterfaceNotSatisfied => "T025",
            DiagCode::T026CannotAssignImmutable => "T026",
            DiagCode::T027MissingStructFields => "T027",
            DiagCode::T028DuplicateFieldInit => "T028",
            DiagCode::T029RecursiveStructInfiniteSize => "T029",
            DiagCode::T030DuplicateFieldDecl => "T030",
            DiagCode::T031Reserved => "T031",
            DiagCode::T032AwaitInvalid => "T032",
            DiagCode::T033IndirectCallNotSupported => "T033",
            DiagCode::T034CannotFormat => "T034",
            DiagCode::T035InvalidDestructor => "T035",
            DiagCode::T036InvalidTestContract => "T036",
            DiagCode::T037InvalidBenchmarkContract => "T037",
            DiagCode::T038IntegerLiteralOutOfRange => "T038",
            DiagCode::T039UnsatisfiedEffect => "T039",
            DiagCode::T040DivisionByZero => "T040",
            DiagCode::T041PrivateFieldAccess => "T041",
            DiagCode::T042UnsupportedComptime => "T042",
            DiagCode::T043ComptimeRuntimeCapture => "T043",
            DiagCode::T044ComptimeEvaluationFailed => "T044",
            DiagCode::T045ComptimeLimitExceeded => "T045",
            DiagCode::T046ComptimeArithmetic => "T046",
            DiagCode::T047InvalidTypeLayout => "T047",
            DiagCode::T048InvalidArrayRepeat => "T048",
            DiagCode::L001LoweringUnresolvedSymbol => "L001",
            DiagCode::G001GenericInstantiationCycle => "G001",
            DiagCode::G002GenericInstantiationLimit => "G002",
            DiagCode::O001UseAfterMove => "O001",
            DiagCode::O002MoveWhileBorrowed => "O002",
            DiagCode::O003MutableBorrowConflict => "O003",
            DiagCode::O004GenerationalFallback => "O004",
            DiagCode::O005DoubleFree => "O005",
            DiagCode::O006DestroyWhileBorrowed => "O006",
            DiagCode::O007InconsistentMoveBetweenBranches => "O007",
            DiagCode::O008UseBeforeInit => "O008",
            DiagCode::O009LifetimeMismatch => "O009",
            DiagCode::O010EscapeOfBorrowedValue => "O010",
            DiagCode::O011FreeRequiresPtr => "O011",
            DiagCode::O012AllocRequiresUnsafe => "O012",
            DiagCode::O013ExternRequiresUnsafe => "O013",
            DiagCode::O014FreeRequiresUnsafe => "O014",
            DiagCode::W001VariableAssignedNotUsed => "W001",
            DiagCode::W002DeadCode => "W002",
            DiagCode::W003UnreachableCode => "W003",
            DiagCode::W004VariableShadowing => "W004",
            DiagCode::W005UnnecessaryMutability => "W005",
            DiagCode::W006UnhandledResult => "W006",
            DiagCode::W007UnusedImport => "W007",
            DiagCode::W008LegacyAnnotationName => "W008",
            DiagCode::U001FeatureNotSupported => "U001",
            DiagCode::ICELX001 => "ICE-LX-001",
            DiagCode::ICEP001 => "ICE-P-001",
            DiagCode::ICEN001 => "ICE-N-001",
            DiagCode::ICET001 => "ICE-T-001",
            DiagCode::ICEO001 => "ICE-O-001",
            DiagCode::ICEL001 => "ICE-L-001",
            DiagCode::ICEGEN001 => "ICE-GEN-001",
            DiagCode::ICEGEN002 => "ICE-GEN-002",
        }
    }
}

impl fmt::Display for DiagCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Compare `DiagCode::ALL` (user-facing) against `docs/errors/*.md`.
///
/// Returns `(missing_docs, orphaned_docs)`. Empty both means bijection holds.
pub fn diag_doc_diff(docs_dir: &std::path::Path) -> (Vec<&'static str>, Vec<String>) {
    use std::collections::BTreeSet;

    let declared: BTreeSet<&'static str> = DiagCode::ALL
        .iter()
        .copied()
        .filter(|c| c.requires_error_doc())
        .map(DiagCode::doc_stem)
        .collect();

    let mut documented = BTreeSet::new();
    if docs_dir.is_dir()
        && let Ok(entries) = std::fs::read_dir(docs_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                documented.insert(stem.to_string());
            }
        }
    }

    let missing: Vec<&'static str> = declared
        .iter()
        .filter(|c| !documented.contains(**c))
        .copied()
        .collect();
    let orphaned: Vec<String> = documented
        .into_iter()
        .filter(|d| !declared.contains(d.as_str()))
        .collect();
    (missing, orphaned)
}

#[cfg(test)]
mod diag_doc_tests {
    use super::*;

    #[test]
    fn all_variants_have_as_str() {
        // Smoke: ALL entries are distinct codes.
        let mut set = std::collections::BTreeSet::new();
        for c in DiagCode::ALL {
            assert!(set.insert(c.as_str()), "duplicate as_str: {}", c.as_str());
        }
        assert_eq!(set.len(), DiagCode::ALL.len());
    }

    #[test]
    fn error_docs_match_diag_code_enum() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let docs = std::path::Path::new(manifest_dir).join("../../docs/errors");
        let (missing, orphaned) = diag_doc_diff(&docs);
        assert!(
            missing.is_empty() && orphaned.is_empty(),
            "DiagCode ↔ docs/errors mismatch.\n  missing docs: {missing:?}\n  orphaned docs: {orphaned:?}\n  run: cargo run -p xtask -- check-diag-docs"
        );
    }
}
