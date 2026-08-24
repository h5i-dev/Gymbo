"""Reverse-mode scalar autodiff — the only numeric engine GYMBO needs.

A GYMBO program's differentiable state is a handful of named scalar PARAMs. The
soft interpreter builds a small graph of `V` nodes over those params during one
bounded run, and `GRAD` calls `backward()` to update the params in place.
"""
from __future__ import annotations

import math


class V:
    """A scalar node in a reverse-mode autodiff graph."""

    __slots__ = ("data", "grad", "_backward", "_prev")

    def __init__(self, data, parents=()):
        self.data = float(data)
        self.grad = 0.0
        self._backward = lambda: None
        self._prev = parents

    def __add__(self, o):
        o = o if isinstance(o, V) else V(o)
        out = V(self.data + o.data, (self, o))
        def _b():
            self.grad += out.grad
            o.grad += out.grad
        out._backward = _b
        return out

    def __sub__(self, o):
        o = o if isinstance(o, V) else V(o)
        out = V(self.data - o.data, (self, o))
        def _b():
            self.grad += out.grad
            o.grad += -out.grad
        out._backward = _b
        return out

    def __mul__(self, o):
        o = o if isinstance(o, V) else V(o)
        out = V(self.data * o.data, (self, o))
        def _b():
            self.grad += o.data * out.grad
            o.grad += self.data * out.grad
        out._backward = _b
        return out

    def sigmoid(self):
        # The one nonlinearity that turns a SIGN into a smooth 0..1 gate, so a
        # comparator can be written branchlessly (see learn_sort4.gym). Compute
        # the stable branch to keep exp() from overflowing on large |data|.
        x = self.data
        if x >= 0.0:
            s = 1.0 / (1.0 + math.exp(-x))
        else:
            e = math.exp(x)
            s = e / (1.0 + e)
        out = V(s, (self,))
        def _b():
            self.grad += s * (1.0 - s) * out.grad
        out._backward = _b
        return out

    def backward(self):
        # Iterative post-order (NOT recursion): a long computation between two
        # GRADs builds a deep graph, and a recursive walk would overflow the
        # Python stack. Each frame is (node, expanded?): first pop pushes the
        # children, the second (after they are done) appends the node.
        topo, seen, stack = [], set(), [(self, False)]
        while stack:
            n, expanded = stack.pop()
            if expanded:
                topo.append(n)
                continue
            if id(n) in seen:
                continue
            seen.add(id(n))
            stack.append((n, True))
            for p in n._prev:
                if id(p) not in seen:
                    stack.append((p, False))
        for n in topo:
            n.grad = 0.0
        self.grad = 1.0
        for n in reversed(topo):
            n._backward()
