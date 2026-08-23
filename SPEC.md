# GYMBO — canonical specification

One language, one parser (`parse`), one differentiable interpreter (`run`), one
hard exporter (`export`) + separate hard interpreter (`run_hard`). This spec
describes exactly what `gymbo.py` implements and what `test_gymbo.py` verifies.
Where it disagrees with the forum debate, this spec (backed by passing tests)
wins.

## 0. Contract

- `run(source, input, max_steps) -> list` — the ONLY external parameters. The
  learning rate, the trainable-cell mask, the loss construction, the target, the
  loops, where `GRAD` fires, and all output live inside the source text.
- `export(source, input, max_steps, grid) -> (hard_source, final_training_loss)`
  — runs soft, then snaps the learned immediates to `grid` and emits a
  self-contained hard PROGRAM TEXT (GRAD -> NOP, LDW -> committed constant).
  `final_training_loss` is `loss_hist[-1]` (how far optimization got); it is NOT
  the rounding gap (see §5).
- `run_hard(hard_source, input, max_steps) -> list` — a separate interpreter with
  plain floats, no autodiff; `GRAD`/`LOSS` are inert and a raw `LDW` is rejected
  (learning is frozen; only exported programs are meant to run here).

## 1. Model

- **Source** = a list of instructions. Each instruction has a discrete opcode and
  at most one **continuous immediate**. Immediates tagged `<value> @group` are the
  **differentiable, self-modifiable source** — the leaves gradient flows to.
- **Execution state** (never a differentiation leaf): accumulator `r0`, unbounded
  integer-addressed tape `M[]` (default 0), data pointer `p`, loss cell `L`,
  program counter, input cursor, output stream.
- Opcodes are discrete on purpose. Self-erasure therefore drives operand
  MAGNITUDE to zero (via a self-referential activity loss), not opcode logits —
  which is why it robustly reaches an inert program (agent-3 showed a task loss on
  opcode logits does *not* prefer NOP; this design sidesteps that entirely).

## 2. Grammar

```
program   := line*
line      := [label ':'] [instr] [';' comment]
label     := identifier
instr     := opcode operand*
immediate := float ['@' group]          ; tagged => trainable source leaf
address   := integer                    ; tape index for LD/ST/SUB
group     := identifier                 ; a named set of trainable slots
```

One instruction per line; `label:` may sit on its own line or prefix an instr;
`;` begins a comment. `JMP`/`JZ` take a label; labels resolve to instruction
indices at parse time.

## 3. ISA

| op | effect (soft interpreter) | typing of its immediate |
|----|---------------------------|-------------------------|
| `LOADI n [@g]` | `r0 = n` (or the leaf, if `@g`) | DATA leaf if `@g`, else const |
| `ADD n [@g]`   | `r0 = r0 + n` | DATA leaf if `@g`, else const |
| `LD a`         | `r0 = M[a]` | — |
| `ST a`         | `M[a] = r0` (stores the node; aliasing) | — |
| `SUB a`        | `r0 = r0 - M[a]` | — |
| `SQ`           | `r0 = r0 * r0` | — |
| `LOSS`         | `L = L + r0` (build the loss in-language) | — |
| `LDW @g`       | `r0 = sum of group g's leaves` (reflection) | reads DATA |
| `GRAD @g eta`  | `L.backward()`; `leaf -= eta*leaf.grad` for g; `L=0`; detach r0/M | writes DATA/STRUCTURAL |
| `JMP t`        | `pc = t` | — |
| `JZ t`         | `if round(r0)==0: pc = t` | — |
| `OUT`          | emit `r0` | — |
| `LDP/STP`      | `r0 = M[p]` / `M[p] = r0` | — |
| `INCP/DECP`    | `p += 1` / `p -= 1` | — |
| `IN`           | `r0 = next input (else 0)` | — |
| `HALT / NOP`   | stop / nothing | — |

Only `GRAD` writes source; `ST` writes only tape (DATA). Sequential PC; `GRAD`
fires when reached; loops use `JMP`/`JZ`. `N1` (no protected objective: the target
lives in ordinary `M[]`) and `N2` (in-band update: the optimizer is an opcode in
the program's own single bounded run) hold in the code as written.

## 4. Gradient semantics (the one correctness rule)

`GRAD` differentiates the loss **functional** the program has accumulated into
`L` through the forward window — `L` is an intermediate node, never a leaf. This
is first-order (`W=0`: the window is the current pass; it does not backprop
through a prior `GRAD`).

`W=0` is now **ENFORCED, not merely relied on** (agent-3, posts 56 & 58). Two
rules make it hold for *any* program, not just the disciplined demos:

1. At the end of every `GRAD` the accumulator `r0` and the whole tape `M[]` are
   detached (re-based to fresh constants), so a value parked in memory and read
   back in a later window cannot backprop through the earlier update.
2. At the *start* of every `GRAD`, the masked group's `.grad` fields are zeroed
   before `backward()`. `backward()` only zeroes nodes reachable from `L`, so a
   masked immediate that is *absent* from the current window's loss would
   otherwise keep a stale gradient from a prior `GRAD` and take a phantom step;
   `dL/dimm` is genuinely 0 for an absent member and the update must be 0.

Only the `@group` immediates persist as leaves across windows — that is exactly
the trainable source state. Higher-order update-under-update (`W>=1`) is designed
but **not implemented here and not claimed as demonstrated** — no mandated
program exercises it.

## 5. Hard export

Snap each learned immediate to `grid`, emit it as a constant, turn `GRAD` into
`NOP`, and turn `LDW @g` into the committed constant `LOADI <sum>`. Every line is
re-emitted with a `__L{idx}:` label so `JMP`/`JZ` targets survive the round-trip;
the result is a self-contained hard program text that `run_hard` re-parses with no
external state.

`export` returns `final_training_loss = loss_hist[-1]` — the soft loss at the last
`GRAD`, a measure of *how far training progressed*, not of rounding. Do **not**
subtract the hard loss from it: that difference (0.396 − 0.253 ≈ 0.14 for
`learn.gym`) is dominated by *un-converged training* (only 8 GRADs), not by
grid-snapping. The **rounding gap** is a separate quantity, defined at the
*committed operand*: `rounding_gap = soft_behavior_loss(w_final) −
hard_behavior_loss(w_snapped)`, i.e. the loss the learned (unsnapped) operand
would incur minus the loss the grid-snapped operand incurs — both evaluated on the
same behavior, with no further learning. For `learn.gym` that is
`(2.49668−3)² − (2.497−3)² ≈ 3.2e-4`, reproduced exactly by
`test_export_learn_commits_learned_value`. Export does **not**
rewrite a 0-valued cell to `NOP` (that would confuse "erased to 0" with
"legitimately 0" — a bug found and removed); an erased cell simply commits
`LOADI 0.0`, which is behaviorally inert.

## 6. The three programs (exact traces, from `test_gymbo.py`, all passing)

- **`learn.gym`** — `OUT = [0, 0.6, 1.08, 1.464, 1.7712, 2.01696, 2.213568,
  2.370854]`, the recursion `v_{n+1}=0.8 v_n + 0.6` to the digit; loss falls
  monotonically 9 → 0.396; `w` moves toward target 3. `export` returns
  `final_training_loss = 0.396` (training stopped at 8 GRADs, still short of
  target 3 — this is convergence progress, not rounding). Export commits
  `w≈2.497`; `run_hard` emits it 8×; the **rounding gap** (loss cost of the
  grid-snap at the committed operand, per §5) `≈ 3.2e-4` — three orders of
  magnitude below the training loss, and the number the test reproduces.
- **`hack.gym`** — `ST` rewrites the target (in `M[0]`) to the behavior before
  `LOSS`; `OUT = [0]*8`, loss `= 0` every pass, `w` frozen (a genuine autodiff
  zero from `w - w` cancellation, confirmed exact by agent-3). Behavior never
  reaches the real target 3 while reported loss is perfect. Export/hard OUT `=
  [0]*8`.
- **`erase.gym`** — behavior `r0 = g` (init 1); `LDW @g` reflectively reads the
  program's own operand; activity loss `g^2`; `OUT = [1, 0.6, 0.36, 0.216, …] →
  6e-5`. Export commits `g≈0`; hard OUT `= [0]*20` — the behavior is nulled to
  the inert program.

## 7. Power ledger (honest; TC now earned by reduction)

- **R1 realizable expressiveness — Turing-complete, DEMONSTRATED.** The shipped
  ISA has conditional branching (`JZ`) + `JMP` and unbounded memory (`M[]` under
  the movable pointer `p`). `bf_to_gymbo` compiles Brainfuck to GYMBO;
  `test_gymbo.py` runs a nested multiplication loop (`6*10 → 60`), a cat program
  (echo input to a `0` sentinel), and `H,I` output — a reduction from a known
  universal machine. This is the one power claim that a run can actually witness,
  and it now does.
- **R2 executed differentiable object** — a bounded-`T` soft run is a finite
  analytic map `immediates → output`; never TC. This is the search surface GRAD
  descends.
- **R3 GRAD step** — `O(window)`, terminates.

## 8. Demonstrated vs designed

- **Demonstrated (tests pass):** N1, N2, useful learning, objective-hacking,
  self-erasure, W=0 enforcement, and Turing-completeness by Brainfuck reduction.
- **Designed but NOT demonstrated (no claim beyond design):** N3 / higher-order
  `W>=1`; self-reproduction (no quine is shipped — `erase.gym` is the "erasure OR
  reproduction" exhibit); the differentiable-quine attracting-manifold analysis.
- **Open (next iteration):** a runnable differentiable quine; concurrency of
  masked regions under a shared PC (control-reachability, not just data-disjoint
  masks) — untested because every shipped program is single-region.

## 9. Removed features (minimality, per post 43/49)

Everything the three programs plus the TC reduction do not need was cut (agent-3,
post 58). Explicitly gone from all earlier drafts:

- **softmax / temperature `tau` / K-way opcode-blend** — opcodes are discrete; no
  continuous op mixing.
- **per-cell presence gate** (soft-blended `write = p*new + (1-p)*old`) —
  **CONSIDERED (agent-1, post 52) AND CUT.** Reason: used by 1 of 3 programs
  (erasure), replaceable by driving an operand to 0 (which passes a test today),
  and its soft-blend perturbs the exact traces unless `p≈1` anyway — i.e. it
  either does nothing or breaks the crisp hard-sequential execution both reviewers
  built (post 43/49 minimality). *Escape hatch:* structural (opcode-deletes-
  itself) erasure, if ever ruled mandatory, is a *localized* gate leaf + one
  `STRUCT` branch in `GRAD` touching only `erase.gym`, never a blend on every op.
- **three-way STRUCTURAL/DATA/EXECUTION typing** — collapsed to: `@group`
  immediates are the only leaves; everything else is execution state.
- **`kind="activity"` / `grad_cell` / external `mask` argument / Python-side
  `passes`** — no demo flags, no external grad target, no mask parameter (the
  trainable set is named in-language by the `@group` tag on `GRAD`), no host-side
  loop; loops are in-language `JMP`/`JZ`. `run` takes only `(source, input,
  max_steps)`.

Not "removed" but explicitly **unclaimed** (see §8): higher-order `W>=1` and a
runnable differentiable quine are designed, not demonstrated, and are asserted
nowhere in the tests.

The exported program is a finite discrete machine (§5): learned immediates snap
to a grid and commit as inert constants; an erased cell is `LOADI 0.0`, inert but
not literally removed.
