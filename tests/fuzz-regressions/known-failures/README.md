# Previously reproduced compiler failures

These sources exposed real backend and optimizer failures while extending the
`synthesized` grammar. Their regressions now live in `tests/regressions/` and
run through the checked multi-backend oracle.

| Regression | Original failure |
| --- | --- |
| `enum-wasm-match.aru` | Wasm runner trapped with `unreachable` for enum payload matching with a conditional join. |
| `enum-optimizer-join.aru` | Synthesized seed 2 returned `-62` at O0 and `0` at O1 in Cranelift. |

The Cranelift failure was fixed by teaching DCE to preserve every reaching
definition of a branch result temp. The Wasm failure was fixed by ordering
terminal trap arms before sibling-reachable joins so stackifier label scopes
remain nested. `enum-if-join.aru` is the minimized Cranelift reproducer; the two
larger sources remain as direct coverage of the original optimizer and Wasm
shapes.
