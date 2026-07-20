# R4 Strict Clippy Baseline Comparison - 2026-07-17

## Result

```text
STRICT_CLIPPY_EXISTING_DEBT
NEW_R4_LINTS=0
```

The strict gate still fails because the verified R3 source baseline already contains Clippy debt. R4 does not add a new diagnostic class or diagnostic location attributable to the R4 changes.

## Commands

```powershell
cargo clippy --release --all-targets --locked --offline --jobs 1 -- -D warnings
cargo clippy --release --tests --locked --offline --jobs 1 -- -D warnings
```

## Count Comparison

| Target | Audited R3 baseline | R4 result | Delta |
|---|---:|---:|---:|
| normal binary | 68 | 68 | 0 |
| test binary | 70 | 70 | 0 |

The terminal summary lines are not counted as diagnostics. The raw files contain 69 and 71 `error:` lines respectively because each includes one final `could not compile ... due to N previous errors` summary.

## Diagnostic Classes

The R4 output retains the R3 classes documented in `docs/R4_CODEBASE_AUDIT_20260717.md`, including:

- derivable `Default` implementations;
- enum variant prefix naming;
- functions with too many arguments;
- manual `is_multiple_of` and range checks;
- collapsible control flow and `while let` suggestions;
- field reassignment after `Default` initialization;
- item ordering in the test target;
- existing iterator, owned-comparison, type-complexity, and slice API suggestions.

The two diagnostics reported in `adaptive_quality.rs` are attached to the pre-existing `AdaptiveMode::default` implementation and pre-existing `AdaptiveAction` variant names. The R4 profile model, ladder transitions, CLI tests, and telemetry additions introduce no new strict diagnostic.

## Evidence

- Normal target raw output: `docs/R4_CLIPPY_RAW_20260717.txt`
- Test target raw output: `docs/R4_CLIPPY_TEST_RAW_20260717.txt`
- R3 audit reference: `docs/R4_CODEBASE_AUDIT_20260717.md`, section 16

No unrelated Clippy cleanup was performed.
