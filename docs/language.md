# The GYMBO language

One language, one parser (`parse`), one differentiable interpreter (`run`), one
hard exporter (`export`) plus a separate hard interpreter (`run_hard`). This file
describes the surface language; [`semantics.md`](semantics.md) describes what the
interpreters do with it.

## Model

- **Source** = parameter declarations plus a list of instructions. Each
  instruction has a discrete opcode and at most one operand.
- **Parameters** are the differentiable, self-modifiable state — the leaves
  gradient flows to. They are declared by name and are the *only* leaves.
- **Execution state** (never a differentiation leaf): accumulator `r0`, an
  unbounded integer-addressed tape `M[]` (default 0), a data pointer `p`, the
  loss cell `L`, the program counter, the input cursor, and the output stream.

Opcodes are discrete on purpose (a real ISA has discrete opcodes and continuous
immediates). Learning moves parameter *magnitudes*, never opcode identities.

## Grammar

```
program   := (decl | directive | line)*
decl      := 'PARAM' name '=' number ['@' group]
directive := 'ENTRY' label | 'DEPLOY' label
line      := [label ':'] [instr] [';' comment]
instr     := opcode operand?
operand   := number | '$' name | '[' integer ']'
```

- One instruction per line; `label:` may sit on its own line or prefix an instr.
- `;` begins a comment.
- `JMP` / `JZ` take a label; labels resolve to instruction indices at parse time.

### Operands — one grammar for every value

| form | meaning | example |
|------|---------|---------|
| `number` | a literal constant | `LOAD 3`, `ADD -1` |
| `$name` | the value of parameter `name` (a source leaf) | `LOAD $w`, `ADD $b` |
| `[k]` | tape cell `M[k]` | `LOAD [0]`, `MUL [1]` |

`LOAD`, `ADD`, `SUB`, and `MUL` all accept any operand form. This replaces the
old `LOADI` / `LD` / `LDW` split: there is now exactly one way to name a value.

Because a parameter is a named leaf, `$g` reads the program's *own source*
directly — reflection needs no special opcode (see `self_silence.gym`).

## Parameters

```
PARAM w = 0.0 @model
PARAM b = 0.0 @model
```

`PARAM name = value @group` declares a trainable scalar `name`, initialised to
`value`, belonging to `group`. `GRAD @group eta` trains exactly the parameters in
that group. The `@group` tag may be omitted for a constant parameter that no
`GRAD` targets.

## Directives

```
ENTRY train      ; the soft (training) run starts here (default: line 0)
DEPLOY predict   ; the exported hard program starts here (default: whole program)
```

`ENTRY` and `DEPLOY` split a program into a training routine and a deployment
routine. `run` starts at `ENTRY`; `export` emits the program from `DEPLOY`
onward with the learned parameters baked in and `GRAD` removed. Programs that do
not learn (e.g. `hello.gym`, `cat.gym`) need neither directive.

## ISA

| op | effect (soft interpreter) |
|----|---------------------------|
| `LOAD o` | `r0 = o` |
| `ADD o` | `r0 = r0 + o` |
| `SUB o` | `r0 = r0 - o` |
| `MUL o` | `r0 = r0 * o` |
| `ST [a]` | `M[a] = r0` (stores the node; aliasing) |
| `SQ` | `r0 = r0 * r0` |
| `SIGMOID` | `r0 = 1 / (1 + e^-r0)` — a branchless differentiable gate |
| `LOSS` | `L = L + r0` (build the loss in-language) |
| `GRAD @g eta` | `L.backward()`; `p -= eta*p.grad` for each param in `g`; `L = 0`; detach `r0`/`M` |
| `JMP t` | `pc = t` |
| `JZ t` | `if round(r0) == 0: pc = t` |
| `OUT` | emit `r0` |
| `IN` | `r0 = next input` (else 0) |
| `LDP` / `STP` | `r0 = M[p]` / `M[p] = r0` |
| `INCP` / `DECP` | `p += 1` / `p -= 1` |
| `HALT` / `NOP` | stop / nothing |

`SIGMOID` is the one nonlinearity besides `SQ`. It turns a *sign* into a smooth
`0..1` value, so a comparator (`min`/`max` of two cells, chosen by a learnable
direction) can be written **without a branch** and stay differentiable — gradient
flows into the direction, and its sign is what gets learned. Because
`round(sigmoid(x)) == 1` iff `x > 0`, the same gate composes with `JZ` to recover
an *exact* branch after export. See [`examples/learn_sort4.gym`](../examples/learn_sort4.gym),
a 4-input sorting network that learns which way each of its five comparators points.

Only `GRAD` writes source (the parameters); `ST` writes only the tape. The
program counter is sequential; `GRAD` fires when the counter reaches it;
repetition is in-language via `JMP` / `JZ`. `LDP` / `STP` / `INCP` / `DECP`
exist for the Brainfuck reduction that witnesses Turing completeness of the hard
ISA.
