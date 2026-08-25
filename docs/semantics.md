# GYMBO semantics

What the interpreters do with the [language](language.md). Where this disagrees
with anyone's intuition, the passing tests in `tests/` win.

## Contract

- `run(source, input, max_steps) -> list` — the SOFT interpreter's OUT stream.
  Its only external parameters are `(source, input, max_steps)`; the learning
  rate, the trainable set (named by `GRAD`), the loss, the targets, the loops,
  and where `GRAD` fires all live in the source. Execution starts at `ENTRY`.
- `run_full(...) -> (out, program, imms, loss_hist)` — the same run, exposing the
  learned parameter nodes and the loss recorded at each `GRAD`.
- `export(source, input, max_steps, grid) -> (hard_source, final_training_loss)`
  — run soft, then emit a self-contained hard program with the learned
  parameters snapped to `grid` and written back into the `PARAM` lines, the
  training routine dropped, and `GRAD` gone.
- `run_hard(hard_source, input, max_steps) -> list` — a SEPARATE interpreter over
  plain floats, no autodiff, with `GRAD` / `LOSS` inert. It runs an exported
  program.

## Gradient semantics — the one correctness rule

`GRAD` differentiates the loss **functional** the program has accumulated into
`L` through the forward window — `L` is an intermediate node, never a leaf. Then
it steps each parameter in the named group by `-eta * grad` and resets `L` to 0.

This is first-order (`W = 0`): the window is the current pass; it does not
backprop through a prior `GRAD`. `W = 0` is **enforced**, not merely relied on.
Two rules make it hold for *any* program, not just the disciplined examples:

1. **Detach after the step.** At the end of every `GRAD` the accumulator `r0` and
   the whole tape `M[]` are re-based to fresh constants, so a value parked in
   memory and read back in a later window cannot backprop through the earlier
   update.
2. **Zero the group first.** At the start of every `GRAD` the group's `.grad`
   fields are zeroed before `backward()`. `backward()` only zeroes nodes
   reachable from `L`, so a group member *absent* from the current window's loss
   would otherwise keep a stale gradient and take a phantom step; `dL/dparam` is
   genuinely 0 for an absent member and its step must be 0.

Only the `@group` parameters persist as leaves across windows — that is exactly
the trainable source state. Higher-order update-under-update (`W ≥ 1`) is
designed but **not implemented here and not claimed as demonstrated**.

## Hard export

Snap each learned parameter to `grid`, rewrite the `PARAM` lines with those
values, and emit the code from `DEPLOY` onward (the whole program if there is no
`DEPLOY`) with `GRAD` turned into `NOP`. Every emitted line is re-labelled
`__L{idx}:` so `JMP` / `JZ` targets survive the round-trip; the result is a
self-contained hard program that `run_hard` re-parses with no external state.

Each `OPCHOICE $s A B o` is committed to a single literal opcode — `A o` if the
learned `s ≤ 0`, else `B o` (a bare `NOP` if the chosen op is `NOP`). Because it
maps one line to one line, instruction indices are preserved and jump targets
still survive. The selector `PARAM` is still emitted; nothing in the exported
body references it, so it is inert — a harmless leftover leaf, like any other
parameter the deploy section does not read.

`export` returns `final_training_loss = loss_hist[-1]` — the soft loss at the
last `GRAD`, i.e. *how far optimization progressed*. Do **not** read it as a
rounding error. For `learn_constant.gym` it is ≈ 0.396 simply because training
stops after 8 `GRAD`s with `w ≈ 2.497`, still short of the target 3.

The **rounding gap** is a separate, much smaller quantity, defined at the
committed operand:

```
rounding_gap = soft_behavior_loss(w_final) − hard_behavior_loss(w_snapped)
```

— the loss the learned (unsnapped) operand would incur minus the loss the
grid-snapped operand incurs, both on the same behavior with no further learning.
For `learn_constant.gym` that is `(2.49668−3)² − (2.497−3)² ≈ 3.2e-4`, three
orders of magnitude below the training loss, and the number
`test_learn_constant_export_is_standalone_predictor` reproduces.

## The example programs

- **`learn_constant.gym`** — `OUT = [0, 0.6, 1.08, 1.464, 1.7712, 2.01696,
  2.213568, 2.370854]`, the update `w ← 0.8 w + 0.6` to the digit; loss falls
  monotonically 9 → 0.396. Export commits `w ≈ 2.497`; the deployed predictor
  emits it once.
- **`fit_affine.gym`** — SGD over an external stream of `(x, y)` pairs from an
  unknown `y = a*x + b`. Recovers `a`, `b` to ~1e-2, and the exported predictor
  generalizes to held-out `x`.
- **`objective_hack.gym`** — the same shape, but it overwrites the target with
  its own prediction before `LOSS`, so the loss is a genuine autodiff zero and
  the parameters never move. Training loss is perfect; the deployed predictor is
  wrong on every held-out `x`. Goodhart, native, in one pass.
- **`self_silence.gym`** — behavior `r0 = g`; the activity loss `g²` is a loss on
  the program's *own operand* (`$g` reads the source leaf directly), so `GRAD`
  drives `g → 0`. The opcodes are untouched — only the operand magnitude goes to
  zero, so this is *silence*, not *erasure*.
- **`learn_op.gym`** — `OPCHOICE $s ADD MUL [0]` learns *which operation* the
  body runs. Trained on `y = x*x` the selector `s` swings positive and export
  commits a literal `MUL [0]`; feed the same skeleton `y = x+x` and `s` swings
  negative and it commits `ADD [0]`. The program does not just tune an operand —
  it rewrites its own instruction.

## Power ledger (honest)

- **Turing-complete, DEMONSTRATED.** The hard ISA has conditional branching
  (`JZ`) + `JMP` and unbounded memory (`M[]` under the movable pointer `p`).
  `bf_to_gymbo` compiles Brainfuck to GYMBO; the tests run a nested
  multiplication loop (`6*10 → 60`), a cat program, and an `H,I` output — a
  reduction from a known universal machine.
- **The executed differentiable object** — a bounded-`T` soft run is a finite,
  **piecewise-differentiable** map `parameters → output`. It is piecewise, not
  globally analytic, because `JZ` branches on `round(r0)`: the control-flow path
  is locally constant, and `GRAD` descends within a piece. This is the search
  surface, and it is never itself Turing-complete. A *decision* can still be made
  differentiable by expressing it as data rather than control flow: `SIGMOID`
  turns a sign into a smooth `0..1` gate, so a comparator's direction (which of
  two cells is emitted) is learned by gradient without ever branching. After
  export the same gate feeds `JZ` to recover an exact branch (see
  `learn_sort4.gym`).
- **The `GRAD` step** — `O(window)`, terminates.

## Demonstrated vs designed

- **Demonstrated (tests pass):** in-language optimization, external-data affine
  fitting, train/deploy separation, objective-hacking, self-silencing, a learned
  branchless sorting network (comparator directions found by gradient, then
  exported as an exact sorter), a **learned opcode** (`OPCHOICE` picks `ADD` vs
  `MUL` from data and exports the literal instruction), `W = 0` enforcement, and
  Turing-completeness by Brainfuck reduction.
- **Designed but not demonstrated:** higher-order `W ≥ 1`; a runnable
  differentiable quine. Asserted nowhere in the tests.
