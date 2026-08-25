# Design notes

Rationale and history, kept out of the [specification](docs/language.md) so the
spec stays a description of what the code does. Where a note here disagrees with
the passing tests, the tests win.

## Provenance

Gymbo was designed and implemented by three coding agents debating on an h5i
forum, then converged onto a single canonical artifact and verified against a
shared test suite. It was originally named nabla-tape during that discussion.

## The v0.2 language reform

The first cut worked but read like a research prototype, not a language. Four
changes made it feel like one — the priority order was: named parameters, `MUL`,
train/deploy directives, and a real external-data example.

### Named parameters replace location-bound immediates

Originally a trainable value was an immediate tagged in place, e.g.
`LOADI 0 @w`, so the parameter was identified by *where it appeared*. Now
parameters are declared up front:

```
PARAM w = 0.0 @model
```

and referenced by name with `$w` anywhere. A value is one thing with one name,
independent of the instructions that read it.

### One operand grammar

`LOADI` (immediate), `LD` (tape), and `LDW` (reflective group read) collapsed
into a single `operand := number | $param | [address]` accepted by `LOAD`,
`ADD`, `SUB`, and `MUL`. `LOAD 3`, `LOAD $w`, `LOAD [0]` are the same opcode.

`LDW @group` — which summed all of a group's leaves — is gone entirely. It was an
unnatural operation that existed only because immediates were location-bound and
you needed *some* way to read "the operand" reflectively. With named parameters,
`$g` already reads the source leaf directly, so reflection is just an ordinary
operand. `self_silence.gym` uses exactly that.

### `MUL` and the affine example

Adding `MUL` made `y = w*x + b` expressible, which unlocked the example the
language was missing: `fit_affine.gym` learns an **unknown** line from an
**external** stream of `(x, y)` pairs, instead of regressing toward a constant
baked into the source. `objective_hack.gym` shares its shape and data but cheats
the loss, so the contrast — genuine learning vs. gaming the objective — falls out
on the same inputs.

### Train / deploy split

`ENTRY` and `DEPLOY` separate the training routine from the predictor. Before,
an exported program still carried its training loop (with `GRAD` turned to
`NOP`), which made the learned artifact awkward to reuse. Now `export` ships only
the `DEPLOY` section with the learned parameters written back into the `PARAM`
lines — a standalone, gradient-free program.

### Renames for honesty

- `erase.gym` → `self_silence.gym`. The program never deleted an instruction; it
  drove an operand's magnitude to zero. "Silence" describes that; "erase"
  overclaimed structural self-modification.
- The executed soft run is described as **piecewise-differentiable**, not an
  "analytic map". `JZ` branches on `round(r0)`, so the map is only differentiable
  within a fixed control-flow path.

## Learnable opcodes (`OPCHOICE`)

`OPCHOICE $s A B o` lets a program learn *which operation* it runs, not just an
operand magnitude. This is a deliberate, scoped **reversal** of the "no
continuous mixing of operations" cut recorded below — worth stating plainly
rather than dressing up as precedent.

- **What it reverses.** During the soft run, `OPCHOICE` *does* continuously mix
  two operations (`r0 = A + sigmoid(s)·(B − A)`). The earlier cut was against
  continuous mixing *per se*; the binary case is still continuous mixing, so the
  old "opcodes are discrete; there is no continuous mixing" line no longer holds
  unqualified. `docs/language.md` was updated to say so.
- **What it keeps.** The mix is **binary**, built from the existing `SIGMOID`
  gate — no softmax, no temperature, no K-way blend. The choice snaps to a
  literal opcode at export, so the hard ISA stays fully discrete, and the
  one-line-in/one-line-out shape leaves instruction indices (and `JMP`/`JZ`
  targets) untouched. It adds a new opcode but **no new state category**: the
  selector is an ordinary `@group` parameter, trained by the ordinary `GRAD`.
- **Why binary, not K-way.** Chaining binary sigmoid gates to select among ≥3
  ops reinvents the softmax that was cut. Binary is the honest minimum that earns
  "programs learn their own instructions"; a general K-way choice is deferred,
  eyes open that it would pull the softmax question back in.
- **Why the restricted opcode set.** `A`, `B` ∈ `{NOP, LOAD, ADD, SUB, MUL}` —
  the ops shaped `r0' = f(r0, operand)`. Blending control flow would require a
  soft program counter (what is "0.3 of a `JMP`?"), which would cost Gymbo its
  smallness. Deferred with `COMMIT` (mid-run rewrite) as the natural next step.

## Removed features (minimality)

Everything the examples plus the TC reduction do not need was cut:

- **softmax / temperature / K-way opcode-blend** — no *continuous, K-way* mixing
  of operations. The binary `OPCHOICE` blend (above) is the one scoped exception;
  softmax over ≥3 opcodes stays cut.
- **per-cell presence gate** (soft-blended `write = p*new + (1-p)*old`) —
  considered and cut. Driving an operand to 0 achieves self-silencing without
  perturbing the crisp hard-sequential execution.
- **three-way STRUCTURAL / DATA / EXECUTION typing** — collapsed to: named
  parameters are the only leaves; everything else is execution state.
- **external mask / grad-cell / host-side training loop** — there are no demo
  flags and no external grad target. The trainable set is named in-language by
  the `@group` on `GRAD`, and loops are in-language `JMP` / `JZ`. `run` takes
  only `(source, input, max_steps)`.

## Open (next iteration)

- Higher-order update-under-update (`W ≥ 1`): designed, not implemented.
- A runnable differentiable quine.
- Continuous operand grids and a richer numeric ISA — deliberately deferred; the
  four reforms above came first.
- `COMMIT @code` — argmax an `OPCHOICE` to a real opcode *mid-run*, so a program
  rewrites its own instructions while executing, not only at export. It needs
  mutable code and a new piece boundary in the soft map (like `JZ`); deferred as
  a heavier semantic step than the `OPCHOICE` + export pair it builds on.
- K-way learnable opcodes — the general form of `OPCHOICE`; deferred because it
  reintroduces the softmax that minimality cut (see above).
