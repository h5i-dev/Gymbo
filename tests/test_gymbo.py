"""Automated tests for GYMBO.

Every learning program runs through the SAME parser and dispatch loop
(`gymbo.run`) with eta/mask/loss/target/loops/GRAD/OUT all in-language. Tests
reproduce the exact traces, check hard-export / hard-interpreter agreement, and
run the Brainfuck TC reduction.

    python3 -m pytest -q
"""
import os

import pytest

import gymbo

HERE = os.path.dirname(__file__)
EX = os.path.join(HERE, os.pardir, "examples")
MAX = 1_000_000


def ex(name):
    return open(os.path.join(EX, name)).read()


def out(name, **kw):
    return gymbo.run(ex(name), max_steps=MAX, **kw)


def approx(xs, ys, tol=1e-9):
    return all(abs(a - b) <= tol for a, b in zip(xs, ys))


# a small linear dataset the fitting programs have NO knowledge of: y = 2x + 1
POINTS = [(-2, -3), (-1, -1), (0, 1), (1, 3), (2, 5)]


def affine_stream(epochs=60):
    s = []
    for _ in range(epochs):
        for x, y in POINTS:
            s += [x, y]
    return s


# ---- basics ----
def test_hello_emits_bytes():
    assert out("hello.gym") == [72.0, 73.0]


def test_cat_echoes_to_sentinel():
    assert out("cat.gym", input=[72, 105, 0]) == [72.0, 105.0]


# ---- learn a constant (the smallest learner) ----
def test_learn_constant_exact_trajectory():
    # eta 0.1 gives the update w <- 0.8 w + 0.6 : trace to the digit.
    got = out("learn_constant.gym")
    assert approx(got, [0.0, 0.6, 1.08, 1.464, 1.7712, 2.01696,
                        2.213568, 2.370854], tol=1e-6)


def test_learn_constant_reduces_loss_monotonically():
    _, _, imms, hist = gymbo.run_full(ex("learn_constant.gym"), max_steps=MAX)
    assert hist == sorted(hist, reverse=True)
    assert abs(imms[0].data - 3.0) < abs(0.0 - 3.0)     # w moved toward target 3


def test_learn_constant_export_is_standalone_predictor():
    # DEPLOY strips the training loop: the hard program is just the frozen value.
    hard, final_training_loss = gymbo.export(ex("learn_constant.gym"),
                                             max_steps=MAX, grid=0.001)
    assert "GRAD" not in hard and "loop" not in hard
    hout = gymbo.run_hard(hard, max_steps=MAX)
    assert hout == [2.497]                               # one snapped constant
    # export's second value is the FINAL TRAINING LOSS, not the rounding gap.
    _, _, imms, hist = gymbo.run_full(ex("learn_constant.gym"), max_steps=MAX)
    assert final_training_loss == hist[-1]
    assert abs(final_training_loss - 0.3958) < 1e-3      # un-converged after 8 GRADs
    rounding_gap = abs((imms[0].data - 3) ** 2 - (hout[0] - 3) ** 2)
    assert abs(rounding_gap - 3.2e-4) < 1e-4             # a separate, tiny quantity
    assert rounding_gap < final_training_loss / 100


# ---- fit an affine map from EXTERNAL data (the main example) ----
def test_fit_affine_learns_unknown_line():
    stream = affine_stream()
    _, _, imms, hist = gymbo.run_full(ex("fit_affine.gym"), input=stream, max_steps=MAX)
    w, b = imms[0].data, imms[1].data
    assert abs(w - 2.0) < 1e-2 and abs(b - 1.0) < 1e-2   # recovered y = 2x + 1
    assert hist[-1] < 1e-3                                # loss driven down


def test_fit_affine_predicts_held_out():
    stream = affine_stream()
    hard, _ = gymbo.export(ex("fit_affine.gym"), input=stream, max_steps=MAX)
    assert "GRAD" not in hard
    for x in (10, -7, 3.5):
        pred = gymbo.run_hard(hard, input=[x], max_steps=MAX)[0]
        assert abs(pred - (2 * x + 1)) < 5e-2            # generalizes off training x


# ---- objective hacking: perfect training loss, useless predictor ----
def test_objective_hack_fools_loss_but_fails_held_out():
    stream = affine_stream()
    _, _, imms, hist = gymbo.run_full(ex("objective_hack.gym"), input=stream, max_steps=MAX)
    assert all(l == 0.0 for l in hist)                   # loss is identically 0
    assert imms[0].data == 0.0 and imms[1].data == 0.0   # params never moved
    hard, _ = gymbo.export(ex("objective_hack.gym"), input=stream, max_steps=MAX)
    for x in (10, -7):
        pred = gymbo.run_hard(hard, input=[x], max_steps=MAX)[0]
        assert pred == 0.0                                # deployed predictor is wrong
        assert abs(pred - (2 * x + 1)) > 1.0


# ---- self-silence: drive the program's own operand to 0 ----
def test_self_silence_monotone_to_zero():
    got = out("self_silence.gym")
    assert got[0] == 1.0
    assert approx(got[:4], [1.0, 0.6, 0.36, 0.216])      # activity-loss decay 0.6^n
    assert all(got[i] > got[i + 1] for i in range(len(got) - 1))
    assert got[-1] < 1e-4
    hard, _ = gymbo.export(ex("self_silence.gym"), max_steps=MAX, grid=0.001)
    assert gymbo.run_hard(hard, max_steps=MAX) == [0.0]   # silenced


# ---- W=0 is ENFORCED, not merely relied on ----
def test_window_is_w0_when_parked_value_read_back():
    # Park a $w-derived node in M, GRAD (detaches the tape), then read it back in
    # a later window: it is a constant there, so dL/dw == 0 and w must not move.
    src = """
        PARAM w = 1.0 @m
        LOAD $w
        ST [5]          ; park the live w node in memory
        SQ
        LOSS
        GRAD @m 0.0     ; eta 0 => w unchanged (=1.0), but tape is detached
        LOAD [5]        ; read the parked value back into a NEW window
        SQ
        LOSS
        GRAD @m 1.0     ; loss is a detached constant => dL/dw == 0 => w stays 1.0
        HALT
    """
    _, _, imms, hist = gymbo.run_full(src, max_steps=MAX)
    assert abs(hist[1] - 1.0) < 1e-12
    assert abs(imms[0].data - 1.0) < 1e-12


def test_no_phantom_step_when_group_absent_from_loss():
    # A masked param present at one GRAD and ABSENT from the next window's loss
    # must NOT take a step from a stale gradient.
    src = """
        PARAM w = 2.0 @m
        LOAD $w
        ST [5]
        LOSS
        GRAD @m 0.1     ; w: 2.0 -> 1.9
        LOAD [5]        ; r0 = detached constant (no dep on w)
        LOSS
        GRAD @m 0.1     ; dL/dw == 0 -> w must STAY 1.9 (not 1.8)
        HALT
    """
    _, _, imms, _ = gymbo.run_full(src, max_steps=MAX)
    assert abs(imms[0].data - 1.9) < 1e-12


# ---- one dispatch loop, no per-program paths ----
def test_same_interpreter_all_programs():
    for f in ("learn_constant.gym", "fit_affine.gym", "objective_hack.gym",
              "self_silence.gym"):
        assert isinstance(gymbo.run(ex(f), affine_stream(), MAX), list)


# ---- unified operand grammar ----
def test_unified_operand_forms():
    # number, [address], and $param all reach the same LOAD/ADD/MUL opcodes.
    src = """
        PARAM w = 3.0 @m
        LOAD 4          ; number
        ST [0]
        LOAD $w         ; $param
        MUL [0]         ; [address] -> 3 * 4
        OUT
        HALT
    """
    assert gymbo.run(src, max_steps=MAX) == [12.0]


# ---- Turing completeness by reduction from Brainfuck ----
def test_bf_multiplication_loop():
    assert gymbo.run_hard(gymbo.bf_to_gymbo("++++++[>++++++++++<-]>."),
                          max_steps=MAX) == [60.0]


def test_bf_cat_with_input():
    prog = gymbo.bf_to_gymbo(",[.,]")
    assert gymbo.run_hard(prog, input=[72, 105, 0], max_steps=MAX) == [72.0, 105.0]


def test_bf_nested_loops_hello_prefix():
    bf = "++++++++[>+++++++++<-]>.+."
    assert gymbo.run_hard(gymbo.bf_to_gymbo(bf), max_steps=MAX) == [72.0, 73.0]
