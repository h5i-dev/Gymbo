"""The one parser for GYMBO.

Design:
  * PARAMs are DECLARED by name, decoupled from any instruction location:
        PARAM w = 0.0 @model
    `w` is a differentiable source leaf; `@model` is the group `GRAD` trains.
  * Every value-taking opcode uses ONE unified operand grammar:
        operand := number | $param | [address]
    so `LOAD 3`, `LOAD $w`, `LOAD [0]` replace the old LOADI / LDW / LD split.
  * ENTRY names where the soft (training) run starts; DEPLOY names where the
    exported hard (prediction) program starts. Either may be omitted.
"""
from __future__ import annotations

# NOARG: no operand. Pointer ops (LDP/STP/INCP/DECP) exist for the Brainfuck
# reduction that witnesses Turing completeness of the hard ISA.
NOARG = {"SQ", "SIGMOID", "LOSS", "OUT", "HALT", "NOP", "IN",
         "LDP", "STP", "INCP", "DECP"}
VALOP = {"LOAD", "ADD", "SUB", "MUL"}   # take a unified operand
JMPOP = {"JMP", "JZ"}                   # take a label


class Instr:
    __slots__ = ("op", "operand", "addr", "group", "eta", "label", "line")

    def __init__(self, op, line):
        self.op = op
        self.operand = None   # (kind, value): kind in {"num","param","addr"}
        self.addr = None      # int tape address for ST
        self.group = None     # group name for GRAD
        self.eta = None       # learning rate for GRAD
        self.label = None     # jump target (name, then resolved to index)
        self.line = line


class Program:
    def __init__(self, code, imm_init, groups, params, slot_name,
                 slot_group, entry, deploy):
        self.code = code            # list[Instr]
        self.imm_init = imm_init    # slot -> initial float
        self.groups = groups        # group name -> list[slot]
        self.params = params        # param name -> slot
        self.slot_name = slot_name  # slot -> param name
        self.slot_group = slot_group  # slot -> group name (or None)
        self.entry = entry          # index the soft run starts at
        self.deploy = deploy        # index the exported program starts at (or None)


def parse_operand(tok, params):
    if tok.startswith("$"):
        name = tok[1:]
        if name not in params:
            raise SyntaxError(f"unknown parameter ${name}")
        return ("param", params[name])
    if tok.startswith("[") and tok.endswith("]"):
        return ("addr", int(tok[1:-1]))
    return ("num", float(tok))


def parse_addr(tok):
    if tok.startswith("[") and tok.endswith("]"):
        return int(tok[1:-1])
    return int(tok)


def parse(source: str) -> Program:
    params, imm_init, groups, slot_name, slot_group = {}, {}, {}, {}, {}
    labels = {}
    entry_label = deploy_label = None
    raw = []

    def new_param(name, init, grp):
        s = len(imm_init)
        imm_init[s] = float(init)
        params[name] = s
        slot_name[s] = name
        slot_group[s] = grp
        if grp:
            groups.setdefault(grp, []).append(s)
        return s

    for line in source.splitlines():
        line = line.split(";", 1)[0].strip()
        if not line:
            continue
        head = line.split()[0].upper()
        if head == "PARAM":
            # PARAM <name> = <value> [@group]
            t = line.split()
            if len(t) < 4 or t[2] != "=":
                raise SyntaxError(f"bad PARAM: {line!r}")
            grp = t[4][1:] if len(t) > 4 and t[4].startswith("@") else None
            new_param(t[1], t[3], grp)
            continue
        if head == "ENTRY":
            entry_label = line.split()[1]
            continue
        if head == "DEPLOY":
            deploy_label = line.split()[1]
            continue
        if line.endswith(":"):
            labels[line[:-1].strip()] = len(raw)
            continue
        if ":" in line.split()[0]:          # "label: OP ..." on one line
            lab, line = line.split(":", 1)
            labels[lab.strip()] = len(raw)
            line = line.strip()
            if not line:
                continue
        raw.append(line)

    code = []
    for i, line in enumerate(raw):
        t = line.split()
        op = t[0].upper()
        a = t[1:]
        ins = Instr(op, line)
        if op in VALOP:
            ins.operand = parse_operand(a[0], params)
        elif op == "ST":
            ins.addr = parse_addr(a[0])
        elif op == "GRAD":
            ins.group = a[0][1:]
            ins.eta = float(a[1])
        elif op in JMPOP:
            ins.label = a[0]
        elif op not in NOARG:
            raise SyntaxError(f"unknown opcode {op!r} on line {i}: {line}")
        code.append(ins)

    for ins in code:                        # resolve labels to indices
        if ins.op in JMPOP:
            ins.label = labels[ins.label]

    entry = labels[entry_label] if entry_label is not None else 0
    deploy = labels[deploy_label] if deploy_label is not None else None
    return Program(code, imm_init, groups, params, slot_name, slot_group,
                   entry, deploy)
