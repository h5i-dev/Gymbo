# Gymbo

**A tiny assembly language whose parameters learn — and rewrite themselves — while the program runs.**

A Gymbo program declares named parameters, builds a loss out of ordinary
instructions, and updates those parameters by gradient descent with an in-band
`GRAD` opcode — as part of execution, not an external training loop. When the run
finishes, `export` writes the learned values back into the source as a
standalone, gradient-free program.

```gymbo
PARAM w = 0.0 @model
PARAM b = 0.0 @model

ENTRY train
DEPLOY predict

train:                  ; SGD over an external stream of (x, y) pairs
        LOAD 300
        ST [2]          ; step budget
loop:
        IN
        ST [0]          ; x
        IN
        ST [1]          ; y
        LOAD $w
        MUL [0]         ; w*x
        ADD $b          ; + b   -> prediction
        SUB [1]         ; - y
        SQ
        LOSS
        GRAD @model 0.01
        LOAD [2]
        ADD -1
        ST [2]
        JZ done
        JMP loop
done:   HALT

predict:                ; the frozen predictor that export ships
        IN
        ST [0]
        LOAD $w
        MUL [0]
        ADD $b
        OUT
        HALT
```

Feed it random `(x, y)` pairs from an unknown line `y = a*x + b` and it recovers
`a` and `b` it was never told, then predicts held-out `x` correctly — see
[`examples/fit_affine.gym`](examples/fit_affine.gym).

## Install & run

```sh
pip install -e .

gymbo run     examples/learn_constant.gym
gymbo export  examples/learn_constant.gym          # prints the hard program
gymbo predict examples/fit_affine.gym --train "…"  --input "10"

python3 -m pytest -q
```

Without installing, prefix commands with `PYTHONPATH=src`.

## What makes it a language, not a demo

- **Named parameters.** `PARAM w = 0.0 @model` declares a differentiable source
  leaf by name, decoupled from where it is used. `$w` reads it anywhere; `@model`
  names the group `GRAD` trains.
- **One operand grammar.** `operand := number | $param | [address]`, so
  `LOAD 3`, `LOAD $w`, and `LOAD [0]` are the same instruction — no `LOADI` /
  `LDW` / `LD` zoo.
- **In-language everything.** The soft interpreter's only external inputs are
  `(source, input, max_steps)`. The learning rate, the trainable set, the loss,
  the targets, the loops, and where `GRAD` fires all live in the program text.
- **Learnable opcodes.** `OPCHOICE $s ADD MUL [0]` learns *which operation* to
  run, not just an operand: gradient trains the selector `$s`, and `export`
  commits the winner to a literal `ADD` or `MUL`. So a program rewrites its own
  instructions, not only its constants.
- **Train, then deploy.** `ENTRY` is where the soft (training) run starts;
  `DEPLOY` is where the exported hard program starts. `export` snaps the learned
  parameters, writes them back into the `PARAM` lines, drops the training loop,
  and emits a self-contained predictor with `GRAD` gone.

## Examples

| file | what it shows |
|------|---------------|
| [`hello.gym`](examples/hello.gym) | the smallest program: emit two bytes |
| [`cat.gym`](examples/cat.gym) | echo input to a `0` sentinel (loops, `JZ`) |
| [`learn_constant.gym`](examples/learn_constant.gym) | the smallest learner: one param toward a target |
| [`fit_affine.gym`](examples/fit_affine.gym) | **the main example** — learn an unknown line from external data, deploy a predictor |
| [`objective_hack.gym`](examples/objective_hack.gym) | same shape, but it cheats the loss and fails on held-out `x` (Goodhart, native) |
| [`self_silence.gym`](examples/self_silence.gym) | a loss on the program's own operand drives its output to 0 |
| [`learn_sort4.gym`](examples/learn_sort4.gym) | a sorting network learns which way each comparator points (`SIGMOID`), then exports an exact sorter |
| [`learn_op.gym`](examples/learn_op.gym) | a program learns its own **opcode** — `ADD` vs `MUL` — from data (`OPCHOICE`), then exports the literal instruction |

`fit_affine.gym` and `objective_hack.gym` run on the *same* data: one truly
learns the rule, the other games the objective and generalizes to nothing.

## Documentation

- [`docs/language.md`](docs/language.md) — grammar, operands, directives, the ISA.
- [`docs/semantics.md`](docs/semantics.md) — gradient semantics (`W=0`), hard
  export, and the Turing-completeness reduction.
- [`DESIGN_NOTES.md`](DESIGN_NOTES.md) — provenance and the design decisions
  (including what was cut and why).

The soft interpreter, hard interpreter, parser, and export live under
[`src/gymbo/`](src/gymbo/); a Brainfuck reduction (`bf_to_gymbo`) witnesses that
the hard ISA is Turing-complete.

## Notice

This project started as a gradient-based symbolic execution for a tiny toy-example language,
and it has shifted to a gradient-based self-modifying language. The old gradient-based
symbolic exeuction engine is still available at [this commit](https://github.com/h5i-dev/Gymbo/tree/69974586031361d3c754e8fad2a9d7a0dad2b3ae).
