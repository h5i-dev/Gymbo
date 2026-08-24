"""GYMBO: a tiny differentiable, self-modifying assembly language.

A program's parameters are declared by name (`PARAM w = 0.0 @model`), an in-band
`GRAD` opcode updates them by gradient descent during the run, and `export`
writes the learned values back into the source as a standalone hard program.
"""
from .autodiff import V
from .brainfuck import bf_to_gymbo
from .export import export
from .parser import parse
from .vm import run, run_full, run_hard

__all__ = ["V", "parse", "run", "run_full", "run_hard", "export", "bf_to_gymbo"]
