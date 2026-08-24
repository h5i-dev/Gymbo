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

## Removed features (minimality)

Everything the examples plus the TC reduction do not need was cut:

- **softmax / temperature / K-way opcode-blend** — opcodes are discrete; there is
  no continuous mixing of operations.
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
