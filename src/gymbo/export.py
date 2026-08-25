"""Hard export.

Run the program soft (training happens in-language), snap each learned PARAM to
`grid`, then emit a self-contained HARD program:

  * the PARAM declarations are rewritten with the LEARNED values — the program
    literally rewrites its own source;
  * if the program declares `DEPLOY`, only the deploy section is emitted (GRAD is
    gone with the training loop), so the result is a standalone predictor;
  * every emitted line is re-labelled `__L{idx}:` so JMP/JZ targets survive the
    round-trip and `run_hard` re-parses with no external state.
"""
from __future__ import annotations

from .parser import JMPOP, VALOP
from .vm import run_full


def _snap(x, grid):
    return round(x / grid) * grid


def _fmt(x):
    return "%g" % x


def _emit_operand(operand, slot_name):
    k, v = operand
    if k == "num":
        return _fmt(v)
    if k == "param":
        return "$" + slot_name[v]
    return f"[{v}]"                        # "addr"


def export(source, input=(), max_steps=10000, grid=0.001):
    """Returns (hard_source_text, final_training_loss).

    `final_training_loss` is `loss_hist[-1]` — the soft loss at the LAST GRAD,
    i.e. how far the in-language optimization progressed. It is NOT the rounding
    gap (the loss cost of snapping the converged-so-far operand to the grid),
    which is a separate, much smaller quantity; see docs/semantics.md.
    """
    _, prog, imms, loss_hist = run_full(source, input, max_steps)

    param_lines = []
    for s in sorted(prog.imm_init):
        val = _snap(imms[s].data, grid)          # commit the learned value
        grp = prog.slot_group.get(s)
        tail = f" @{grp}" if grp else ""
        param_lines.append(f"PARAM {prog.slot_name[s]} = {_fmt(val)}{tail}")

    start = prog.deploy if prog.deploy is not None else 0
    body = prog.code[start:]
    lines = []
    for ins in body:
        if ins.op == "GRAD":
            lines.append("NOP")                  # learning frozen at export
        elif ins.op == "OPCHOICE":
            # Commit the learned choice to a literal opcode. round(sigmoid(s))==1
            # iff s>0, so this matches the soft blend's argmax at the boundary.
            # Occupies exactly one line in and one line out, so instruction
            # indices (and thus JMP/JZ targets) are preserved.
            chosen = ins.op_b if imms[ins.sel[1]].data > 0 else ins.op_a
            if chosen == "NOP":
                lines.append("NOP")
            else:
                lines.append(f"{chosen} {_emit_operand(ins.operand, prog.slot_name)}")
        elif ins.op in VALOP:
            lines.append(f"{ins.op} {_emit_operand(ins.operand, prog.slot_name)}")
        elif ins.op == "ST":
            lines.append(f"ST [{ins.addr}]")
        elif ins.op in JMPOP:
            # Targets are re-based to the deploy body. A jump into the region
            # BEFORE `start` (e.g. deploy sharing code with train) would emit a
            # negative label and silently produce a hard program that dies with
            # a KeyError in run_hard. Fail loudly here instead.
            if not (start <= ins.label <= len(prog.code)):
                raise ValueError(
                    f"{ins.op} on line {ins.line!r} targets code outside the "
                    f"deploy section; the exported predictor must be "
                    f"self-contained (no jumps into the pre-DEPLOY region)."
                )
            lines.append(f"{ins.op} __L{ins.label - start}")
        else:
            lines.append(ins.op)

    labelled = [f"__L{idx}: {ln}" for idx, ln in enumerate(lines)]
    hard_src = "\n".join(param_lines + [""] + labelled)
    final_training_loss = loss_hist[-1] if loss_hist else 0.0
    return hard_src, final_training_loss
