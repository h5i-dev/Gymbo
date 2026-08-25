"""The two interpreters.

  * `run_full` / `run` — the SOFT interpreter. Params are autodiff leaves; `GRAD`
    updates them in place during the run. Its ONLY external parameters are
    (source, input, max_steps): the learning rate, the trainable set (the group
    named by `GRAD`), the loss, the targets, the loops, and where `GRAD` fires
    all live inside the program text. Execution begins at ENTRY (else line 0).
  * `run_hard` — a SEPARATE interpreter over plain floats, no autodiff, `GRAD`
    frozen. It runs an exported (hard) program.
"""
from __future__ import annotations

import math

from .autodiff import V
from .parser import parse


def _sigmoid(x):
    if x >= 0.0:
        return 1.0 / (1.0 + math.exp(-x))
    e = math.exp(x)
    return e / (1.0 + e)


def _apply(op, r0, x):
    """The CHOICEABLE opcodes as pure r0' = f(r0, x). Uses only + - *, so the
    same function serves the soft (V nodes) and hard (floats) interpreters."""
    if op == "NOP":
        return r0
    if op == "LOAD":
        return x
    if op == "ADD":
        return r0 + x
    if op == "SUB":
        return r0 - x
    return r0 * x                          # MUL


def run_full(source, input=(), max_steps=10000):
    """Differentiable interpreter. Returns (output, program, imms, loss_hist)."""
    prog = parse(source)
    imms = {s: V(v) for s, v in prog.imm_init.items()}   # continuous source leaves
    M, out, loss_hist = {}, [], []
    r0 = V(0.0)
    L = V(0.0)
    p = 0
    inp = list(input)
    ip = 0
    pc = prog.entry
    steps = 0
    code = prog.code

    def mem(a):
        return M.get(a, V(0.0))

    def val(operand):
        k, v = operand
        if k == "num":
            return V(v)
        if k == "param":
            return imms[v]
        return mem(v)                     # "addr"

    while pc < len(code) and steps < max_steps:
        ins = code[pc]
        pc += 1
        steps += 1
        op = ins.op
        if op == "LOAD":
            r0 = val(ins.operand)
        elif op == "ADD":
            r0 = r0 + val(ins.operand)
        elif op == "SUB":
            r0 = r0 - val(ins.operand)
        elif op == "MUL":
            r0 = r0 * val(ins.operand)
        elif op == "OPCHOICE":
            # Soft blend of two opcodes, gated by sigmoid of the learnable
            # selector. Same shape as the sort4 comparator (a + g*(b-a)):
            # gradient flows into the selector and its SIGN is what gets learned.
            x = val(ins.operand)
            a = _apply(ins.op_a, r0, x)
            b = _apply(ins.op_b, r0, x)
            g = val(ins.sel).sigmoid()
            r0 = a + g * (b - a)
        elif op == "ST":
            M[ins.addr] = r0
        elif op == "SQ":
            r0 = r0 * r0
        elif op == "SIGMOID":
            r0 = r0.sigmoid()
        elif op == "LOSS":
            L = L + r0
        elif op == "GRAD":
            # Zero the masked group's grads FIRST: backward() only zeroes nodes
            # reachable from L, so a group member ABSENT from this window's loss
            # would otherwise keep a stale .grad from a prior GRAD and take a
            # phantom step. dL/dparam is genuinely 0 for an absent member.
            for s in prog.groups.get(ins.group, ()):
                imms[s].grad = 0.0
            L.backward()
            for s in prog.groups.get(ins.group, ()):
                imms[s].data -= ins.eta * imms[s].grad
            loss_hist.append(L.data)
            L = V(0.0)
            # W=0: detach execution state so the next window cannot backprop
            # through this update. Only the params persist as leaves; r0 and the
            # tape are re-based to fresh constants.
            r0 = V(r0.data)
            M = {a: V(v.data) for a, v in M.items()}
        elif op == "JMP":
            pc = ins.label
        elif op == "JZ":
            if round(r0.data) == 0:
                pc = ins.label
        elif op == "OUT":
            out.append(r0.data)
        elif op == "LDP":
            r0 = mem(p)
        elif op == "STP":
            M[p] = r0
        elif op == "INCP":
            p += 1
        elif op == "DECP":
            p -= 1
        elif op == "IN":
            r0 = V(inp[ip]) if ip < len(inp) else V(0.0)
            ip += 1
        elif op == "HALT":
            break
        elif op == "NOP":
            pass
    return out, prog, imms, loss_hist


def run(source, input=(), max_steps=10000):
    """Canonical entry: only (source, input, max_steps). Returns the OUT stream."""
    return run_full(source, input, max_steps)[0]


def run_hard(hard_source, input=(), max_steps=10000):
    """SEPARATE hard interpreter: plain floats, no autodiff, GRAD/LOSS inert.
    Runs a committed (exported) program. Execution starts at line 0 (export
    emits the deploy section at the top)."""
    prog = parse(hard_source)
    params = dict(prog.imm_init)          # slot -> float
    M, out = {}, []
    r0 = 0.0
    p = 0
    inp = list(input)
    ip = 0
    pc = prog.entry
    steps = 0
    code = prog.code

    def mem(a):
        return M.get(a, 0.0)

    def val(operand):
        k, v = operand
        if k == "num":
            return v
        if k == "param":
            return params[v]
        return mem(v)                     # "addr"

    while pc < len(code) and steps < max_steps:
        ins = code[pc]
        pc += 1
        steps += 1
        op = ins.op
        if op == "LOAD":
            r0 = val(ins.operand)
        elif op == "ADD":
            r0 = r0 + val(ins.operand)
        elif op == "SUB":
            r0 = r0 - val(ins.operand)
        elif op == "MUL":
            r0 = r0 * val(ins.operand)
        elif op == "OPCHOICE":
            # An exported program has OPCHOICE already committed to a concrete
            # opcode, so this is only reached when run_hard is handed an
            # un-exported program: commit by argmax (sel > 0 -> op_b).
            x = val(ins.operand)
            chosen = ins.op_b if val(ins.sel) > 0 else ins.op_a
            r0 = _apply(chosen, r0, x)
        elif op == "ST":
            M[ins.addr] = r0
        elif op == "SQ":
            r0 = r0 * r0
        elif op == "SIGMOID":
            r0 = _sigmoid(r0)
        elif op == "LOSS":
            pass
        elif op == "JMP":
            pc = ins.label
        elif op == "JZ":
            if round(r0) == 0:
                pc = ins.label
        elif op == "OUT":
            out.append(r0)
        elif op == "LDP":
            r0 = mem(p)
        elif op == "STP":
            M[p] = r0
        elif op == "INCP":
            p += 1
        elif op == "DECP":
            p -= 1
        elif op == "IN":
            r0 = float(inp[ip]) if ip < len(inp) else 0.0
            ip += 1
        elif op == "HALT":
            break
        elif op in ("NOP", "GRAD"):
            pass
    return out
