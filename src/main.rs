use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use serde_json::json;

use jsonl_shuffle::{shuffle_file, Options, MAX_THREADS};

/// Shuffle the lines of a JSONL file into a new file, in parallel.
///
/// The same seed and the same kept lines give the same file on any number of threads, and the
/// file that Python's random.Random(seed).shuffle gives over the kept lines in file order.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// The JSONL file to read. It is never changed.
    input: PathBuf,
    /// The file to write. An existing file is refused.
    output: PathBuf,
    #[arg(long, default_value_t = 20_260_929)]
    seed: u64,
    /// Keep only lines that contain this text. Repeat it, and a line must contain every one.
    #[arg(long)]
    contains: Vec<String>,
    /// Leave out lines that contain this text. Repeat it, and any one leaves a line out.
    #[arg(long)]
    omits: Vec<String>,
    /// A string field, such as workflow, whose values are counted in each part of the output.
    #[arg(long)]
    tally: Option<String>,
    /// How many equal parts of the output the counts are given for: 2 for two writers.
    #[arg(long, default_value_t = 2)]
    parts: usize,
    /// Worker threads. Capped at 14, whatever is asked for.
    #[arg(long, default_value_t = MAX_THREADS)]
    threads: usize,
    /// Where the stats go. By default, beside the output as <stem>_shuffle.json.
    #[arg(long)]
    stats: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let started = std::time::Instant::now();
    let options = Options {
        seed: args.seed,
        contains: args.contains.iter().map(|s| s.as_bytes().to_vec()).collect(),
        omits: args.omits.iter().map(|s| s.as_bytes().to_vec()).collect(),
        tally: args.tally.clone(),
        parts: args.parts,
        threads: args.threads,
    };
    let report = shuffle_file(&args.input, &args.output, &options)?;
    let seconds = started.elapsed().as_secs_f64();

    let widest = report.tally.iter().max_by(|a, b| {
        let spread = |c: &Vec<u64>| {
            let (lo, hi) = (*c.iter().min().unwrap(), *c.iter().max().unwrap());
            (hi - lo) as f64 / (c.iter().sum::<u64>().max(1)) as f64
        };
        spread(&a.1).total_cmp(&spread(&b.1))
    });
    let stats_path = args.stats.clone().unwrap_or_else(|| {
        let stem = args.output.with_extension("");
        PathBuf::from(format!("{}_shuffle.json", stem.display()))
    });
    let stats = json!({
        "generator": "jsonl-shuffle",
        "input": std::fs::canonicalize(&args.input)?.display().to_string(),
        "output": std::fs::canonicalize(&report.output)?.display().to_string(),
        "seed": args.seed,
        "contains": args.contains,
        "omits": args.omits,
        "lines": report.lines,
        "kept": report.kept,
        "left_out": report.left_out,
        "bytes": report.bytes,
        "checksum": format!("{:016x}", report.checksum),
        "threads": report.threads,
        "parts": report.parts,
        "seconds": {
            "index": round(report.seconds_index),
            "shuffle": round(report.seconds_shuffle),
            "write": round(report.seconds_write),
            "total": round(seconds),
        },
        "tally": args.tally.as_ref().map(|field| json!({
            "field": field,
            "values": report.tally.len(),
            "widest": widest.map(|(v, c)| json!({"value": v, "counts": c})),
            "counts": report.tally.iter().map(|(v, c)| (v.clone(), json!(c)))
                .collect::<serde_json::Map<_, _>>(),
        })),
    });
    std::fs::write(&stats_path, format!("{}\n", serde_json::to_string_pretty(&stats)?))?;

    let parts: Vec<String> = report.parts.iter().map(u64::to_string).collect();
    print!("{} lines shuffled with seed {} into {} ({} left out), parts {}",
           report.kept, args.seed, args.output.display(), report.left_out, parts.join("/"));
    if let Some((value, counts)) = widest {
        let counts: Vec<String> = counts.iter().map(u64::to_string).collect();
        print!(", {} values, widest split {} {}", report.tally.len(), value, counts.join("/"));
    }
    println!("; checksum matches; {:.1} GB in {:.1}s ({} threads)",
             report.bytes as f64 / 1e9, seconds, report.threads);
    println!("stats: {}", stats_path.display());
    Ok(())
}

fn round(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}
