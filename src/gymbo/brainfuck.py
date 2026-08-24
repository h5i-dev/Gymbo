"""Brainfuck -> GYMBO: a reduction that witnesses Turing completeness of the
hard ISA.

Data pointer = p; cell = M[p]; I/O via IN/OUT. The ISA's JZ+JMP (branching) and
the unbounded M[] under the movable pointer (unbounded memory) are sufficient:
`[` = if M[p]==0 skip to loop end; `]` = re-test at top.
"""
from __future__ import annotations


def bf_to_gymbo(bf: str) -> str:
    src, stack = [], []
    for c in bf:
        if c == ">":
            src.append("INCP")
        elif c == "<":
            src.append("DECP")
        elif c == "+":
            src += ["LDP", "ADD 1", "STP"]
        elif c == "-":
            src += ["LDP", "ADD -1", "STP"]
        elif c == ".":
            src += ["LDP", "OUT"]
        elif c == ",":
            src += ["IN", "STP"]
        elif c == "[":
            lid = len(src)
            stack.append(lid)
            src += [f"B{lid}:", "LDP", f"JZ E{lid}"]
        elif c == "]":
            lid = stack.pop()
            src += ["LDP", f"JZ E{lid}", f"JMP B{lid}", f"E{lid}: NOP"]
    src.append("HALT")
    return "\n".join(src)
