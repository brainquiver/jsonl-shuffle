---
type: Repository Guide
title: JSONL Shuffle
description: Rust based JSONL shuffler with multithreading that reproduces Python's random.shuffle order for a given seed.
status: stable
tags: [data, jsonl, rust]
generated:
  by: claude-code/opus-5.5
  at: 2026-09-29T04:30:00Z
edited:
  by: claude-code/opus-5.5
  at: 2026-10-02T17:54:03Z
---

# JSONL Shuffle

This repository shuffles the lines of a JSONL file into a new file. It keeps only the lines that contain, or do not contain, given text, and it counts the values of one field in each part of the output. It is for a dataset build that splits one input between several writers, where no writer may get more of one kind of row than another.

What sets it apart from a shuffle in a script is two things. The order is CPython's: for the same seed and the same kept lines, the file is byte for byte the one that `random.Random(seed).shuffle` over those lines in file order gives, so a file a Python script shuffled can be rebuilt, checked or replaced here. And the work is parallel: the workers index the input at once, and each writes its own stretch of the output as one sequential block while the random reads run many at a time.

It reads one file, of lines up to 4 GiB each and up to 4,294,967,295 kept lines. The filters match bytes and do not parse JSON.

| What it does | Where |
|---|---|
| CPython's generator and shuffle | `src/mt.rs` |
| Index, shuffle, write and check | `src/lib.rs` |
| The command line and the stats | `src/main.rs` |
| The tests, with CPython's reference values | `tests/shuffle.rs` |

**The same seed and the same kept lines give the same file on any number of threads, and it is the file Python's `random.Random(seed).shuffle` gives.**

## 1. Build and Run

    cargo build --release
    cargo test --release

    target/release/jsonl-shuffle IN.jsonl OUT.jsonl
    target/release/jsonl-shuffle IN.jsonl OUT.jsonl --contains '"prose":{' --tally workflow

| Flag | Default | What it does |
|---|---|---|
| `--seed` | 20260929 | the seed of the order |
| `--contains` | none | keep a line only if it holds this text; repeat it, and a line must hold every one |
| `--omits` | none | leave a line out if it holds this text; repeat it, and any one leaves the line out |
| `--tally` | none | a string field whose values are counted in each part of the output, such as `workflow` |
| `--parts` | 2 | how many equal parts the counts are given for |
| `--threads` | 14 | worker threads, capped at 14 |
| `--stats` | `<stem>_shuffle.json` | where the stats go |

A blank line is never kept and never counted.

## 2. Directory Tree

    src/          the library, the generator and the command line
    tests/        the tests

## 3. Concepts

**A kept line** is a line that is not blank and that passes the filters. The order is a shuffle of the kept lines in the order they appear in the input.

**A part** is one of `--parts` equal stretches of the output. Part k starts at line floor(k x kept / parts), so two parts split where `--start 0 --limit kept/2` and `--start kept/2` split.

**The checksum** is the wrapping sum of the xxh3 of every kept line, without its line break. It does not depend on order, so the input and the output give the same sum when every kept line was written exactly once.

## 4. Rules

**An existing output is refused.** A shuffled file is often an input that later runs find lines in by position, and a file rewritten under them gives every position another line. Move the old file away to write a new one.

**The output is written to `OUT.partial` and renamed only after the check.** The check compares the count, the size and the checksum of what was written with what was indexed, so a file named `OUT` always holds every kept line once. A failed run leaves `OUT.partial` as it is.

**Never more than 14 threads.** Every tool on the drive holds to 14 workers, and `--threads` is capped there whatever it is given.

**The order is CPython's, and a change to `src/mt.rs` changes every file.** The seeding, `getrandbits`, the rejection in `_randbelow` and the loop from the last index down are each CPython's. The tests hold them to reference values that Python 3.14 produced, so a change that breaks the match fails there.

## 5. Outputs

A run writes `OUT`, and the stats beside it. It prints one line:

    3999999 lines shuffled with seed 20260929 into OUT (4000001 left out), parts 1999999/2000000, 39 values, widest split answer-grounding 51041/51524; checksum matches; 18.7 GB in 51.4s (14 threads)

The stats hold the input and the output paths, the seed and the filters, the lines, kept and left out, the bytes, the checksum, the threads, the lines in each part, the seconds of each pass, and, with `--tally`, the count of each value in each part and the value whose parts differ most.

## 6. Limitations

| Limitation | Reason |
|---|---|
| One input file | a shuffle across files is a shuffle of their concatenation, which a caller can make |
| Byte filters, not JSON | the filters run on every line of files of tens of gigabytes, and a parse would cost most of the time |
| `--tally` reads a string value only, the first one of that name in the line | it finds `"field": "value"` by bytes, which is what a flat top-level field needs |
| Lines up to 4 GiB, kept lines up to 4,294,967,295 | an index entry is 12 bytes, and a larger one would cost memory every run pays |
| The input must not change during a run | it is memory-mapped; a change shows as a checksum mismatch, and no output is named |

## 7. Benchmarks

An Apple M5 Pro with 25.8 GB of memory, a release build, 14 threads. The input on an external PCIe SSD, the output on the internal SSD. The input is the eight-million-row build of classify-typed-general-finetune, and the filter keeps the rows that hold prose.

| Run | Lines | Kept | Output | Index | Write | Total |
|---|---:|---:|---:|---:|---:|---:|
| `jsonl-shuffle` | 8,000,000 | 3,999,999 | 18.7 GB | 17.9 s | 33.5 s | 51.4 s |
| a single-threaded Python shuffle of the same lines | 8,000,000 | 3,999,999 | 18.7 GB | | | 5 min 51 s at most |

The two outputs are byte for byte the same file.
