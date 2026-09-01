#!/usr/bin/env python3
"""Aggregate rope_latticeMetrics samples for pre/post-P2B comparison.

Reads all sample-*.json files in a directory (default: script dir),
extracts the three histograms (head_guard_wait, head_guard_hold,
flusher_wait) plus per-op counters, and prints:

  1. Sample count, capture window
  2. head_guard_hold aggregate stats (mean of means, max of maxes,
     total observations, distribution across buckets)
  3. Per-op aggregate stats
  4. A one-line summary suitable for pasting into the deploy log

Usage:
    python3 summarise.py                         # aggregate ./sample-*.json
    python3 summarise.py post-p2b                # aggregate post-p2b/sample-*.json
    python3 summarise.py pre.json post.json      # explicit files (compare 2)

Zero external deps - stdlib only.
"""
from __future__ import annotations

import glob
import json
import os
import sys
from pathlib import Path
from statistics import mean, median


def load_samples(pattern):
    """Load every JSON file matching pattern. Returns list of decoded dicts."""
    samples = []
    for p in sorted(glob.glob(pattern)):
        try:
            with open(p) as f:
                d = json.load(f)
            # accept either the raw RPC response or just the .result payload
            if "result" in d and isinstance(d["result"], dict):
                samples.append({"path": p, "payload": d["result"]})
            elif "head_guard_hold" in d:
                samples.append({"path": p, "payload": d})
        except (json.JSONDecodeError, OSError) as e:
            print(f"[warn] skipping {p}: {e}", file=sys.stderr)
    return samples


def summarise_histogram(samples, key):
    """Aggregate a named histogram field across samples."""
    counts, means, maxes, sums = [], [], [], []
    bucket_totals = None
    for s in samples:
        h = s["payload"].get(key)
        if not h:
            continue
        counts.append(h.get("count", 0))
        means.append(h.get("mean_ns", 0))
        maxes.append(h.get("max_ns", 0))
        sums.append(h.get("sum_ns", 0))
        bc = h.get("bucket_counts", [])
        if bucket_totals is None:
            bucket_totals = list(bc)
        elif len(bucket_totals) == len(bc):
            bucket_totals = [a + b for a, b in zip(bucket_totals, bc)]
    return {
        "samples": len(counts),
        "obs_total": sum(counts),
        "mean_of_means_ns": mean(means) if means else 0,
        "median_of_maxes_ns": median(maxes) if maxes else 0,
        "max_of_maxes_ns": max(maxes) if maxes else 0,
        "sum_of_sums_ns": sum(sums),
        "bucket_totals": bucket_totals or [],
    }


def summarise_per_op(samples):
    """Aggregate per-op counters keyed by op name across samples."""
    agg = {}
    for s in samples:
        per_op = s["payload"].get("per_op", [])
        for op in per_op:
            name = op.get("op", "?")
            e = agg.setdefault(name, {
                "samples": 0,
                "acquired_total": 0,
                "hold_ns_total": 0,
                "wait_ns_total": 0,
                "mean_hold_ns_samples": [],
                "mean_wait_ns_samples": [],
            })
            e["samples"] += 1
            e["acquired_total"] += op.get("acquired", 0)
            e["hold_ns_total"] += op.get("hold_ns_total", 0)
            e["wait_ns_total"] += op.get("wait_ns_total", 0)
            e["mean_hold_ns_samples"].append(op.get("mean_hold_ns", 0))
            e["mean_wait_ns_samples"].append(op.get("mean_wait_ns", 0))
    return {
        name: {
            "samples": v["samples"],
            "acquired_total": v["acquired_total"],
            "hold_ns_total": v["hold_ns_total"],
            "wait_ns_total": v["wait_ns_total"],
            "mean_of_mean_hold_ns": mean(v["mean_hold_ns_samples"]) if v["mean_hold_ns_samples"] else 0,
            "mean_of_mean_wait_ns": mean(v["mean_wait_ns_samples"]) if v["mean_wait_ns_samples"] else 0,
        }
        for name, v in agg.items()
    }


def ns_to_ms(x):
    return x / 1_000_000.0


def print_summary(label, samples):
    print(f"\n=== {label} ===")
    print(f"  files loaded: {len(samples)}")
    if not samples:
        print("  no data")
        return

    for hist_key in ("head_guard_wait", "head_guard_hold", "flusher_wait"):
        h = summarise_histogram(samples, hist_key)
        print(f"\n  {hist_key}:")
        print(f"    obs_total:         {h['obs_total']:,}")
        print(f"    mean_of_means:     {ns_to_ms(h['mean_of_means_ns']):.4f} ms")
        print(f"    median_of_maxes:   {ns_to_ms(h['median_of_maxes_ns']):.4f} ms")
        print(f"    max_of_maxes:      {ns_to_ms(h['max_of_maxes_ns']):.4f} ms")
        if h["bucket_totals"]:
            b = h["bucket_totals"]
            total = sum(b) or 1
            print(f"    bucket distribution (percent):")
            for i, cnt in enumerate(b):
                pct = 100.0 * cnt / total
                marker = "*" * int(pct / 2)
                print(f"      bucket[{i:2d}]  {cnt:>10,}  {pct:5.1f}%  {marker}")

    per_op = summarise_per_op(samples)
    print("\n  per_op:")
    for name, v in sorted(per_op.items()):
        print(f"    {name}:")
        print(f"      acquired:         {v['acquired_total']:,}")
        print(f"      mean_hold:        {ns_to_ms(v['mean_of_mean_hold_ns']):.4f} ms")
        print(f"      mean_wait:        {ns_to_ms(v['mean_of_mean_wait_ns']):.4f} ms")


def main(argv):
    args = argv[1:] or [os.path.dirname(os.path.abspath(__file__))]

    # If args look like explicit files, load each separately for compare.
    if all(a.endswith(".json") and os.path.isfile(a) for a in args):
        for a in args:
            print_summary(a, load_samples(a))
        return 0

    # Otherwise treat args as directories.
    for d in args:
        pat = os.path.join(d, "sample-*.json")
        print_summary(d, load_samples(pat))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
