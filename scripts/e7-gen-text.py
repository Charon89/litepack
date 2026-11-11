"""E7-5 (CI's `installer` job): write about <MiB> MiB of text-like data, fast and the same on every run (R334).

Words of a fixed random vocabulary (seed 334), Zipf-weighted, in lines: no chunk repeats, so lpk's dedup cannot
shorten it, and single-threaded LZMA (`lpk a --balanced --threads 1`) takes minutes on a few hundred MiB.

    python -I scripts/e7-gen-text.py <out> <MiB>
"""

import random
import sys


def main(argv):
    if len(argv) != 2:
        raise SystemExit(__doc__)
    out, mib = argv[0], int(argv[1])
    r = random.Random(334)
    vocab = ["".join(r.choices("etaoinshrdlucmfwypvbgkjqxz", k=r.randint(2, 10))) for _ in range(20000)]
    weights = [1 / (i + 1) for i in range(len(vocab))]
    with open(out, "wb") as f:
        n = 0
        while n < mib * 1024 * 1024:
            words = r.choices(vocab, weights, k=200000)
            for i in range(0, len(words), 12):
                words[i] += "\n" if i % 96 == 0 else ","
            b = " ".join(words).encode()
            f.write(b)
            n += len(b)


if __name__ == "__main__":
    main(sys.argv[1:])
