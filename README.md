# Gymbo

A tiny differentiable, self-modifying assembly language. The source code is
continuous state, and a `GRAD` instruction performs a gradient update to that
source as part of execution, not an external training procedure.

There is one parser, one differentiable (soft) interpreter, one hard
interpreter, and one export. The soft interpreter's only external execution
parameters are `source`, `input`, and `max_steps`. Everything else (the learning
rate, the update masks, the loss construction, the targets, the loops, and the
location of `GRAD`) lives inside the program. The program counter is a sequential
integer, `GRAD` fires only when the counter reaches it, repetition is in-language
via conditional jumps, and observable output uses an `OUT` instruction.

See [`SPEC.md`](SPEC.md) for the canonical specification.

## Files

- `gymbo.py` — parser, soft interpreter, hard interpreter, export, and a
  Brainfuck reduction (`bf_to_gymbo`) that witnesses Turing completeness of the
  hard instruction set.
- `SPEC.md` — the canonical specification.
- `learn.gym` — useful learning: the program learns the recursion `v <- 0.8 v + 0.6`.
- `hack.gym` — objective-hacking: the loss is driven identically to zero while the
  behavior stays frozen and never reaches the real target.
- `erase.gym` — self-erasure: the program drives its own activity to zero and
  exports to an inert program.
- `test_gymbo.py` — automated tests that reproduce the exact traces, check the
  hard export and rounding gap, and run the Brainfuck Turing-completeness witness.

## Run

```
python3 gymbo.py learn.gym       # or hack.gym / erase.gym
python3 -m pytest -q             # 13 tests
```

## What is demonstrated

The three behaviors run through the same parser and the same dispatch loop, with
no per-demo execution paths: useful learning, objective-hacking, and
self-erasure. The hard, exported instruction set has conditional branching and an
unbounded tape, and a Brainfuck reduction witnesses its Turing completeness.
Hard-language expressiveness and the imperfect reachability of discrete programs
by gradient descent are kept separate: the tests report the rounding gap rather
than assuming it is zero.

## Provenance

Gymbo was designed and implemented by three coding agents debating on an h5i
forum, then converged onto a single canonical artifact and verified against a
shared test suite. It was originally named nabla-tape during that discussion.
