#!/usr/bin/env python3
"""GYMBO: a tiny differentiable, self-modifying language.

ONE syntax, ONE parser, ONE soft (differentiable) interpreter whose only
external parameters are (source, input, max_steps). Everything else -- the
learning rate, which cells are trainable (the GRAD mask), how the loss is
constructed, the target, the loops, where GRAD fires, and all observable
output -- lives inside the program text.

The differentiable *source* is the set of continuous instruction IMMEDIATES
(tagged `<value> @group`). GRAD is an in-band opcode: when the sequential PC
reaches it, it differentiates the loss the program has accumulated so far and
updates the masked immediates in place. That is the self-modification: the
program rewrites its own operands during its own execution.

Opcodes are discrete (a real ISA has discrete opcodes, continuous immediates).
Self-erasure therefore drives operand MAGNITUDE to zero via a self-referential
activity loss (LDW reflectively reads the program's own source), NOT opcode
logits -- which sidesteps the "task loss on logits does not prefer NOP" failure.

Exports: `export()` hard-rounds immediates to a grid and commits them as
constants (GRAD -> NOP; an erased cell simply commits `LOADI 0.0`, inert but not
literally NOP); `run_hard()` is a SEPARATE interpreter (plain floats, no
autodiff, GRAD is frozen) that runs the committed program.
"""
from __future__ import annotations
import sys

# ------------------------------------------------------------------ autodiff
class V:
    """A scalar node in a reverse-mode autodiff graph."""
    __slots__ = ("data", "grad", "_backward", "_prev")
    def __init__(self, data, parents=()):
        self.data = float(data); self.grad = 0.0
        self._backward = lambda: None; self._prev = parents
    def __add__(self, o):
        o = o if isinstance(o, V) else V(o)
        out = V(self.data + o.data, (self, o))
        def _b(): self.grad += out.grad; o.grad += out.grad
        out._backward = _b; return out
    def __sub__(self, o):
        o = o if isinstance(o, V) else V(o)
        out = V(self.data - o.data, (self, o))
        def _b(): self.grad += out.grad; o.grad += -out.grad
        out._backward = _b; return out
    def __mul__(self, o):
        o = o if isinstance(o, V) else V(o)
        out = V(self.data * o.data, (self, o))
        def _b(): self.grad += o.data * out.grad; o.grad += self.data * out.grad
        out._backward = _b; return out
    def backward(self):
        topo, seen = [], set()
        def build(n):
            if id(n) not in seen:
                seen.add(id(n))
                for p in n._prev: build(p)
                topo.append(n)
        build(self)
        for n in topo: n.grad = 0.0
        self.grad = 1.0
        for n in reversed(topo): n._backward()

# ------------------------------------------------------------------ parser
NOARG = {"SQ", "LOSS", "OUT", "HALT", "NOP", "LDP", "STP", "INCP", "DECP", "IN"}
IMMOP = {"LOADI", "ADD"}          # carry a (possibly learnable) immediate
ADDROP = {"LD", "ST", "SUB"}      # carry an integer tape address
JMPOP = {"JMP", "JZ"}             # carry a label

class Instr:
    __slots__ = ("op", "num", "slot", "group", "addr", "label", "eta", "line")
    def __init__(self, op, line):
        self.op = op; self.num = None; self.slot = None; self.group = None
        self.addr = None; self.label = None; self.eta = None; self.line = line

class Program:
    def __init__(self, code, imm_init, groups):
        self.code = code            # list[Instr]
        self.imm_init = imm_init    # slot -> initial float
        self.groups = groups        # group name -> list[slot]

def parse(source: str) -> Program:
    labels, raw = {}, []
    for line in source.splitlines():
        line = line.split(";", 1)[0].strip()
        if not line: continue
        if line.endswith(":"):
            labels[line[:-1].strip()] = len(raw); continue
        # allow "label: OP ..." on one line
        if ":" in line.split()[0]:
            lab, line = line.split(":", 1)
            labels[lab.strip()] = len(raw); line = line.strip()
            if not line: continue
        raw.append(line)
    code, imm_init, groups = [], {}, {}
    def new_slot(init, grp):
        s = len(imm_init); imm_init[s] = float(init)
        groups.setdefault(grp, []).append(s); return s
    for i, line in enumerate(raw):
        t = line.split(); op = t[0].upper(); ins = Instr(op, line)
        a = t[1:]
        if op in IMMOP:
            ins.num = float(a[0])
            if len(a) > 1 and a[1].startswith("@"):
                ins.group = a[1][1:]; ins.slot = new_slot(a[0], ins.group)
        elif op in ADDROP:
            ins.addr = int(a[0])
        elif op == "LDW":
            ins.group = a[0][1:]
        elif op == "GRAD":
            ins.group = a[0][1:]; ins.eta = float(a[1])
        elif op in JMPOP:
            ins.label = a[0]
        elif op not in NOARG:
            raise SyntaxError(f"unknown opcode {op!r} on line {i}: {line}")
        code.append(ins)
    for ins in code:                       # resolve labels to indices
        if ins.op in JMPOP: ins.label = labels[ins.label]
    return Program(code, imm_init, groups)

# ------------------------------------------------------------------ soft interpreter
def run_full(source, input=(), max_steps=10000):
    """Differentiable interpreter. Returns (output, final_program, imms, loss_hist)."""
    prog = parse(source)
    imms = {s: V(v) for s, v in prog.imm_init.items()}   # continuous source leaves
    M, out, loss_hist = {}, [], []
    r0 = V(0.0); L = V(0.0); p = 0; inp = list(input); ip = 0
    pc = steps = 0; code = prog.code
    def mem(a): return M.get(a, V(0.0))
    while pc < len(code) and steps < max_steps:
        ins = code[pc]; pc += 1; steps += 1; op = ins.op
        if op == "LOADI":   r0 = imms[ins.slot] if ins.slot is not None else V(ins.num)
        elif op == "ADD":   r0 = r0 + (imms[ins.slot] if ins.slot is not None else V(ins.num))
        elif op == "LD":    r0 = mem(ins.addr)
        elif op == "ST":    M[ins.addr] = r0
        elif op == "SUB":   r0 = r0 - mem(ins.addr)
        elif op == "SQ":    r0 = r0 * r0
        elif op == "LOSS":  L = L + r0
        elif op == "LDW":
            acc = V(0.0)
            for s in prog.groups.get(ins.group, ()): acc = acc + imms[s]
            r0 = acc
        elif op == "GRAD":
            # Zero the masked group's grads FIRST (agent-3, post 58): backward()
            # only zeroes nodes reachable from L, so a group member ABSENT from
            # this window's loss would otherwise keep a stale .grad from a prior
            # GRAD and take a phantom step. dL/dimm is 0 for an absent member.
            for s in prog.groups.get(ins.group, ()): imms[s].grad = 0.0
            L.backward()
            for s in prog.groups.get(ins.group, ()):
                imms[s].data -= ins.eta * imms[s].grad
            loss_hist.append(L.data); L = V(0.0)
            # ENFORCE W=0 (agent-3, post 56): detach execution state so the next
            # window cannot backprop through this update. imms persist as the
            # only leaves; r0 and the tape are re-based to fresh constants.
            r0 = V(r0.data)
            M = {a: V(v.data) for a, v in M.items()}
        elif op == "JMP":   pc = ins.label
        elif op == "JZ":
            if round(r0.data) == 0: pc = ins.label
        elif op == "OUT":   out.append(r0.data)
        elif op == "LDP":   r0 = mem(p)
        elif op == "STP":   M[p] = r0
        elif op == "INCP":  p += 1
        elif op == "DECP":  p -= 1
        elif op == "IN":    r0 = V(inp[ip]) if ip < len(inp) else V(0.0); ip += 1
        elif op == "HALT":  break
        elif op == "NOP":   pass
    return out, prog, imms, loss_hist

def run(source, input=(), max_steps=10000):
    """Canonical entry: only (source, input, max_steps). Returns the OUT stream."""
    return run_full(source, input, max_steps)[0]

# ------------------------------------------------------------------ hard export + hard interpreter
def _snap(x, grid): return round(x / grid) * grid

def export(source, input=(), max_steps=10000, grid=0.001):
    """Run soft, then hard-round the LEARNED immediates to `grid` and commit them
    as constants. GRAD -> NOP (learning frozen). A cell that snapped to 0 commits
    `LOADI 0.0` -- behaviorally inert, but NOT rewritten to NOP: conflating
    "erased to 0" with "structurally removed" was a bug (found and removed).

    Returns (hard_source_text, final_training_loss). `final_training_loss` is
    `loss_hist[-1]` -- the soft loss at the LAST GRAD, i.e. how far the in-language
    optimization progressed. It is NOT the snap-rounding gap: it can be large
    simply because training had not converged (e.g. learn.gym stops at 8 GRADs
    with loss ~0.396 while w is still ~2.497, short of target 3). The rounding
    gap -- the loss cost of snapping the *converged-so-far* operand to the grid --
    is a separate, much smaller quantity (~3e-4 for learn.gym); see SPEC section 5
    and test_export_learn_commits_learned_value for its exact definition."""
    _, prog, imms, loss_hist = run_full(source, input, max_steps)
    lines = []
    for ins in prog.code:
        if ins.slot is not None:                     # learnable immediate cell
            val = _snap(imms[ins.slot].data, grid)   # commit the learned value
            lines.append(f"{ins.op} {val}")          # (val==0 => behaviorally inert)
        elif ins.op == "GRAD":
            lines.append("NOP")                      # learning frozen at export
        elif ins.op == "LDW":
            val = _snap(sum(imms[s].data for s in prog.groups.get(ins.group, ())), grid)
            lines.append(f"LOADI {val}")             # reflective read -> committed const
        elif ins.op in JMPOP:
            lines.append(f"{ins.op} __L{ins.label}")
        elif ins.op in ADDROP:
            lines.append(f"{ins.op} {ins.addr}")
        elif ins.op in IMMOP:
            lines.append(f"{ins.op} {ins.num}")
        else:
            lines.append(ins.op)
    # re-attach labels as standalone lines so JMP targets survive the round-trip
    out = []
    for idx, ln in enumerate(lines):
        out.append(f"__L{idx}: {ln}")
    hard_src = "\n".join(out)
    final_training_loss = loss_hist[-1] if loss_hist else 0.0
    return hard_src, final_training_loss

def run_hard(hard_source, input=(), max_steps=10000):
    """SEPARATE hard interpreter: plain floats, no autodiff, GRAD is inert.
    Runs the committed (exported) program."""
    prog = parse(hard_source)
    M, out = {}, []; r0 = 0.0; p = 0; inp = list(input); ip = 0
    pc = steps = 0; code = prog.code
    while pc < len(code) and steps < max_steps:
        ins = code[pc]; pc += 1; steps += 1; op = ins.op
        if op == "LOADI":   r0 = ins.num
        elif op == "ADD":   r0 = r0 + ins.num
        elif op == "LD":    r0 = M.get(ins.addr, 0.0)
        elif op == "ST":    M[ins.addr] = r0
        elif op == "SUB":   r0 = r0 - M.get(ins.addr, 0.0)
        elif op == "SQ":    r0 = r0 * r0
        elif op == "LOSS":  pass
        elif op == "JMP":   pc = ins.label
        elif op == "JZ":
            if round(r0) == 0: pc = ins.label
        elif op == "OUT":   out.append(r0)
        elif op == "LDP":   r0 = M.get(p, 0.0)
        elif op == "STP":   M[p] = r0
        elif op == "INCP":  p += 1
        elif op == "DECP":  p -= 1
        elif op == "IN":    r0 = float(inp[ip]) if ip < len(inp) else 0.0; ip += 1
        elif op == "HALT":  break
        elif op in ("NOP", "GRAD"): pass          # GRAD is frozen in hard runs
        elif op == "LDW":
            # LDW is a soft-only reflective read; export rewrites it to LOADI.
            # Seeing a raw LDW here means an un-exported program -- fail loudly
            # rather than silently mis-execute (agent-3, post 56).
            raise ValueError("run_hard: raw LDW -- run export() first")
    return out

# ------------------------------------------------------------------ Brainfuck -> GYMBO (TC reduction)
def bf_to_gymbo(bf: str) -> str:
    """Compile Brainfuck to GYMBO. Data pointer = p; cell = M[p]; I/O via IN/OUT.
    A reduction from a known universal machine: the ISA's JZ+JMP (branching) and
    unbounded M[] under the movable pointer (unbounded memory) are sufficient for
    Turing completeness. `[` = if M[p]==0 skip to loop end; `]` = re-test at top."""
    src, stack = [], []
    for c in bf:
        if c == ">":   src.append("INCP")
        elif c == "<": src.append("DECP")
        elif c == "+": src += ["LDP", "ADD 1", "STP"]
        elif c == "-": src += ["LDP", "ADD -1", "STP"]
        elif c == ".": src += ["LDP", "OUT"]
        elif c == ",": src += ["IN", "STP"]
        elif c == "[":
            lid = len(src); stack.append(lid)
            src += [f"B{lid}:", "LDP", f"JZ E{lid}"]
        elif c == "]":
            lid = stack.pop()
            src += ["LDP", f"JZ E{lid}", f"JMP B{lid}", f"E{lid}: NOP"]
    src.append("HALT")
    return "\n".join(src)

if __name__ == "__main__":
    for f in sys.argv[1:]:
        print(f"== {f} ==")
        print(run(open(f).read(), max_steps=100000))
