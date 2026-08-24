"""Command line: `gymbo run|export|predict FILE [--input "1 2 3"]`."""
from __future__ import annotations

import argparse

from .export import export
from .vm import run, run_hard

MAX = 1_000_000


def _nums(s):
    return [float(x) for x in s.split()] if s.strip() else []


def main(argv=None):
    ap = argparse.ArgumentParser(prog="gymbo",
                                 description="A tiny differentiable, "
                                             "self-modifying assembly language.")
    sub = ap.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("run", help="soft (training) run; prints the OUT stream")
    r.add_argument("file")
    r.add_argument("--input", default="")

    e = sub.add_parser("export", help="train, then print the hard program")
    e.add_argument("file")
    e.add_argument("--input", default="")

    p = sub.add_parser("predict", help="train, export, then run the predictor")
    p.add_argument("file")
    p.add_argument("--train", default="", help="training data fed to IN")
    p.add_argument("--input", default="", help="held-out inputs for the predictor")

    args = ap.parse_args(argv)
    src = open(args.file).read()

    if args.cmd == "run":
        print(run(src, _nums(args.input), max_steps=MAX))
    elif args.cmd == "export":
        hard, loss = export(src, _nums(args.input), max_steps=MAX)
        print(hard)
        print(f"\n; final_training_loss = {loss:g}")
    elif args.cmd == "predict":
        hard, _ = export(src, _nums(args.train), max_steps=MAX)
        print(run_hard(hard, _nums(args.input), max_steps=MAX))


if __name__ == "__main__":
    main()
