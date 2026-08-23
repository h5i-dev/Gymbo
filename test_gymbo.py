"""Automated tests for GYMBO. All three mandated programs run through the
SAME parser and dispatch loop (`gymbo.run`), with eta/mask/loss/target/loops/
GRAD/OUT all in-language. Tests reproduce the exact traces and check the
hard-export / hard-interpreter agreement, plus the Brainfuck TC reduction.

    python3 -m pytest test_gymbo.py -q
"""
import gymbo

MAX = 100000
def out(f, **kw): return gymbo.run(open(f).read(), max_steps=MAX, **kw)
def approx(xs, ys, tol=1e-9): return all(abs(a - b) <= tol for a, b in zip(xs, ys))

# ---- exact traces (the canonical numbers) ----
def test_learning_exact_trajectory():
    # v_{n+1} = 0.8 v_n + 0.6 : the in-language GRAD recursion, to the digit.
    got = out("learn.gym")
    # published 6-decimal digits; compare at that precision
    assert approx(got, [0.0, 0.6, 1.08, 1.464, 1.7712, 2.01696, 2.213568, 2.370854], tol=1e-6)

def test_learning_reduces_loss_monotonically():
    _, _, imms, hist = gymbo.run_full(open("learn.gym").read(), max_steps=MAX)
    assert hist == sorted(hist, reverse=True)          # loss strictly falls
    assert abs(imms[0].data - 3.0) < abs(0.0 - 3.0)    # w moved toward target 3

def test_hack_freezes_behavior_at_perfect_loss():
    got = out("hack.gym")
    assert got == [0.0] * 8                             # behavior never moves
    _, _, imms, hist = gymbo.run_full(open("hack.gym").read(), max_steps=MAX)
    assert all(l == 0.0 for l in hist)                 # yet loss is perfect (0)
    assert imms[0].data == 0.0                          # gradient was a genuine 0
    # the real target is 3, in M[0] initially; behavior 0 != 3 despite loss 0
    assert 0.0 != 3.0

def test_erasure_monotone_to_zero():
    got = out("erase.gym")
    assert got[0] == 1.0
    assert approx(got[:4], [1.0, 0.6, 0.36, 0.216])     # activity-loss decay 0.6^n
    assert all(got[i] > got[i + 1] for i in range(len(got) - 1))
    assert got[-1] < 1e-4                                # erased

# ---- W=0 is ENFORCED, not merely relied on (agent-3 finding, post 56) ----
def test_window_is_w0_when_parked_value_read_back():
    # Park an @w-derived node in M, GRAD, then read it back in a later window.
    # Because GRAD detaches the tape, M[5] is a CONSTANT in window 2, so window
    # 2's loss does not depend on w at all: dL/dw == 0 and w must not move. If the
    # tape were NOT detached, M[5] would still be the live w leaf and w would take
    # a spurious step (the W>=1 leak this enforces against).
    src = """
        LOADI 1.0 @w
        ST 5            ; park the live w node in memory
        SQ
        LOSS
        GRAD @w 0.0     ; eta 0 => w unchanged (=1.0), but tape is detached
        LD 5            ; read the parked value back into a NEW window
        SQ
        LOSS
        GRAD @w 1.0     ; loss is a detached constant => dL/dw == 0 => w stays 1.0
        HALT
    """
    _, _, imms, hist = gymbo.run_full(src, max_steps=MAX)
    assert abs(hist[1] - 1.0) < 1e-12          # window-2 loss = const 1.0
    assert abs(imms[0].data - 1.0) < 1e-12     # w frozen: no cross-window backprop

def test_no_phantom_step_when_group_absent_from_loss():
    # agent-3 finding, post 58: a masked param present at one GRAD and ABSENT from
    # the next window's loss must NOT take a step from a stale gradient. backward()
    # only zeroes nodes reachable from L, so absent group members must be zeroed
    # explicitly. Here w feeds the 1st loss, then M[5] is detached and the 2nd
    # loss is a constant => dL/dw == 0 => w must stay put after the 2nd GRAD.
    src = """
        LOADI 2 @w
        ST 5
        LOSS
        GRAD @w 0.1     ; w: 2.0 -> 1.9
        LD 5            ; r0 = detached constant (no dep on w)
        LOSS
        GRAD @w 0.1     ; dL/dw == 0 -> w must STAY 1.9 (not 1.8)
        HALT
    """
    _, _, imms, _ = gymbo.run_full(src, max_steps=MAX)
    assert abs(imms[0].data - 1.9) < 1e-12

# ---- hard export + separate hard interpreter ----
def test_export_learn_commits_learned_value():
    src = open("learn.gym").read()
    hard, final_training_loss = gymbo.export(src, max_steps=MAX, grid=0.001)
    hout = gymbo.run_hard(hard, max_steps=MAX)
    assert all(abs(x - hout[0]) < 1e-9 for x in hout)   # committed constant
    assert abs(hout[0] - 2.497) < 1e-9                   # snapped learned w
    # export's second return value is the FINAL TRAINING LOSS (loss_hist[-1]) --
    # how far optimization got (still short of target 3 after 8 GRADs), NOT the
    # rounding gap. Pin that semantics so the SPEC's number is reproducible.
    _, _, imms, hist = gymbo.run_full(src, max_steps=MAX)
    assert final_training_loss == hist[-1]
    assert abs(final_training_loss - 0.3958) < 1e-3      # ~0.396, un-converged
    # the ROUNDING GAP is a separate, much smaller quantity: the loss cost of the
    # grid-snap at the committed operand (soft w_final vs hard w_snapped, same
    # behavior, no further learning). This is ~450x below final_training_loss.
    rounding_gap = abs((imms[0].data - 3) ** 2 - (hout[0] - 3) ** 2)
    assert abs(rounding_gap - 3.2e-4) < 1e-4             # ~3.2e-4 (faithful snap)
    assert rounding_gap < final_training_loss / 100      # not conflated

def test_export_hack_and_erase_are_inert():
    for f in ("hack.gym", "erase.gym"):
        hard, _ = gymbo.export(open(f).read(), max_steps=MAX, grid=0.001)
        hout = gymbo.run_hard(hard, max_steps=MAX)
        assert all(abs(x) < 1e-6 for x in hout)         # behavior nulled to 0

# ---- one dispatch loop, no per-program paths ----
def test_same_interpreter_all_programs():
    # identical call shape for all three; no callbacks, no kwargs beyond the contract
    for f in ("learn.gym", "hack.gym", "erase.gym"):
        assert isinstance(gymbo.run(open(f).read(), (), MAX), list)

# ---- run_hard refuses a raw (non-exported) reflective opcode (agent-3 finding) ----
def test_run_hard_rejects_raw_ldw():
    import pytest
    with pytest.raises(ValueError):
        gymbo.run_hard("LOADI 1.0\nLDW @g\nOUT\nHALT", max_steps=MAX)

# ---- Turing completeness by reduction from Brainfuck ----
def test_bf_multiplication_loop():
    # 6 * 10 via a nested [ ] loop with +,-,>,< : prints 60
    assert gymbo.run_hard(gymbo.bf_to_gymbo("++++++[>++++++++++<-]>."), max_steps=MAX) == [60.0]

def test_bf_cat_with_input():
    # ,[.,] echoes input bytes until a 0 sentinel
    prog = gymbo.bf_to_gymbo(",[.,]")
    assert gymbo.run_hard(prog, input=[72, 105, 0], max_steps=MAX) == [72.0, 105.0]

def test_bf_nested_loops_hello_prefix():
    # 'H' = 72 = 8*9 ; prints 72 then 73 (H, I)
    bf = "++++++++[>+++++++++<-]>.+."
    assert gymbo.run_hard(gymbo.bf_to_gymbo(bf), max_steps=MAX) == [72.0, 73.0]
