"""Reverse-mode scalar autodiff — the only numeric engine GYMBO needs.

A GYMBO program's differentiable state is a handful of named scalar PARAMs. The
soft interpreter builds a small graph of `V` nodes over those params during one
bounded run, and `GRAD` calls `backward()` to update the params in place.
"""
from __future__ import annotations


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

    def backward(self):
        topo, seen = [], set()
        def build(n):
            if id(n) not in seen:
                seen.add(id(n))
                for p in n._prev:
                    build(p)
                topo.append(n)
        build(self)
        for n in topo:
            n.grad = 0.0
        self.grad = 1.0
        for n in reversed(topo):
            n._backward()
