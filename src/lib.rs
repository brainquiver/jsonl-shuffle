//! Shuffle the lines of a JSONL file into a new file, in parallel.
//!
//! Three passes. The index pass maps the input and splits it at line breaks into chunks that
//! the workers scan at once, keeping the offset and length of each line the filters keep, an
//! order-free checksum of those lines, and the value of a tallied field. The shuffle is CPython's
//! over the kept lines in file order, so the order depends on the seed and the kept lines alone,
//! never on the number of workers. The write pass splits the shuffled order into stretches of
//! about equal size, and each worker copies its stretch from the map into a buffer and writes it
//! at its own offset in the output, so the random reads run many at a time and every write is
//! one sequential block.
//!
//! The output is written to `<output>.partial`, checked against the checksum and the count, and
//! renamed only if every kept line was written exactly once. An output that already exists is
//! refused, because a file other runs find lines in by position must never be rewritten.

pub mod mt;

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use memchr::memmem::Finder;
use memmap2::{Advice, Mmap};
use rayon::prelude::*;
use xxhash_rust::xxh3::xxh3_64;

/// The most workers any run takes, whatever is asked for.
pub const MAX_THREADS: usize = 14;

const WRITE_BUFFER: usize = 8 << 20;

pub struct Options {
    pub seed: u64,
    /// A line is kept only if it contains every one of these.
    pub contains: Vec<Vec<u8>>,
    /// A line is left out if it contains any of these.
    pub omits: Vec<Vec<u8>>,
    /// A string field whose values are counted in each part of the output.
    pub tally: Option<String>,
    /// How many equal parts the output is counted in.
    pub parts: usize,
    pub threads: usize,
}

pub struct Report {
    pub output: PathBuf,
    /// Lines that are not blank.
    pub lines: u64,
    pub kept: u64,
    pub left_out: u64,
    pub bytes: u64,
    /// The wrapping sum of the xxh3 of every kept line, without its line break.
    pub checksum: u64,
    pub threads: usize,
    /// Kept lines in each part of the output. Part k starts at floor(k * kept / parts).
    pub parts: Vec<u64>,
    /// Each tallied value with its count in each part, in order of first appearance.
    pub tally: Vec<(String, Vec<u64>)>,
    pub seconds_index: f64,
    pub seconds_shuffle: f64,
    pub seconds_write: f64,
}

struct Chunk {
    offsets: Vec<u64>,
    lengths: Vec<u32>,
    values: Vec<u32>,
    names: Vec<Vec<u8>>,
    checksum: u64,
    lines: u64,
    left_out: u64,
}

/// The value of the first `"field": "value"` in a line, without its quotes.
pub fn field_value<'a>(line: &'a [u8], key: &Finder) -> Option<&'a [u8]> {
    let mut from = 0;
    while let Some(found) = key.find(&line[from..]) {
        let mut p = from + found + key.needle().len();
        while p < line.len() && line[p].is_ascii_whitespace() {
            p += 1;
        }
        if p < line.len() && line[p] == b':' {
            p += 1;
            while p < line.len() && line[p].is_ascii_whitespace() {
                p += 1;
            }
            if p >= line.len() || line[p] != b'"' {
                return None;
            }
            let start = p + 1;
            let mut q = start;
            while q < line.len() && line[q] != b'"' {
                q += if line[q] == b'\\' { 2 } else { 1 };
            }
            return Some(&line[start..q.min(line.len())]);
        }
        from += found + 1;
    }
    None
}

/// Where each chunk starts: every `count`-th of the file, moved on to the start of a line.
fn chunk_starts(map: &[u8], count: usize) -> Vec<usize> {
    let mut starts = vec![0];
    for c in 1..count {
        let at = map.len() * c / count;
        let start = match memchr::memchr(b'\n', &map[at..]) {
            Some(i) => at + i + 1,
            None => map.len(),
        };
        if start > *starts.last().unwrap() && start < map.len() {
            starts.push(start);
        }
    }
    starts.push(map.len());
    starts.dedup();
    starts
}

fn index_chunk(map: &[u8], start: usize, end: usize, contains: &[Finder], omits: &[Finder],
               tally: Option<&Finder>) -> Result<Chunk> {
    let mut chunk = Chunk { offsets: Vec::new(), lengths: Vec::new(), values: Vec::new(),
                            names: Vec::new(), checksum: 0, lines: 0, left_out: 0 };
    let mut ids: HashMap<&[u8], u32> = HashMap::new();
    let mut pos = start;
    while pos < end {
        let line_end = memchr::memchr(b'\n', &map[pos..end]).map_or(end, |i| pos + i);
        let line = &map[pos..line_end];
        let at = pos;
        pos = line_end + 1;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        chunk.lines += 1;
        if !contains.iter().all(|f| f.find(line).is_some()) || omits.iter().any(|f| f.find(line).is_some()) {
            chunk.left_out += 1;
            continue;
        }
        let length = u32::try_from(line.len()).context("a line longer than 4 GiB")?;
        chunk.offsets.push(at as u64);
        chunk.lengths.push(length);
        chunk.checksum = chunk.checksum.wrapping_add(xxh3_64(line));
        if let Some(key) = tally {
            let value = field_value(line, key).unwrap_or(b"");
            let next = ids.len() as u32;
            let id = *ids.entry(value).or_insert_with(|| {
                chunk.names.push(value.to_vec());
                next
            });
            chunk.values.push(id);
        }
    }
    Ok(chunk)
}

struct Stretch {
    first: usize,
    last: usize,
    offset: u64,
}

/// The shuffled order cut into `count` stretches of about equal bytes, each with the offset in
/// the output where it starts.
fn stretches(order: &[u32], lengths: &[u32], total: u64, count: usize) -> Vec<Stretch> {
    let mut out = Vec::with_capacity(count);
    let (mut first, mut offset, mut written) = (0usize, 0u64, 0u64);
    for (pos, &row) in order.iter().enumerate() {
        written += u64::from(lengths[row as usize]) + 1;
        let boundary = total * (out.len() as u64 + 1) / count as u64;
        if written >= boundary && out.len() + 1 < count {
            out.push(Stretch { first, last: pos + 1, offset });
            first = pos + 1;
            offset = written;
        }
    }
    if first < order.len() || out.is_empty() {
        out.push(Stretch { first, last: order.len(), offset });
    }
    out
}

pub fn shuffle_file(input: &Path, output: &Path, options: &Options) -> Result<Report> {
    if output.exists() {
        bail!("{} exists. A shuffled file is written once: move it away to write another.",
              output.display());
    }
    let threads = options.threads.clamp(1, MAX_THREADS);
    let parts = options.parts.max(1);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build()?;
    let file = File::open(input).with_context(|| format!("open {}", input.display()))?;
    if file.metadata()?.len() == 0 {
        bail!("{} is empty", input.display());
    }
    // SAFETY: the input is only read, and a file changed under a running shuffle is outside
    // what this tool promises; the checksum would then refuse the output.
    let map = unsafe { Mmap::map(&file)? };
    let _ = map.advise(Advice::Sequential);

    let started = Instant::now();
    let contains: Vec<Finder> = options.contains.iter().map(|n| Finder::new(n).into_owned()).collect();
    let omits: Vec<Finder> = options.omits.iter().map(|n| Finder::new(n).into_owned()).collect();
    let key = options.tally.as_ref().map(|f| format!("\"{f}\"").into_bytes());
    let key = key.as_ref().map(|k| Finder::new(k).into_owned());
    let starts = chunk_starts(&map, threads * 8);
    let chunks: Vec<Chunk> = pool.install(|| {
        starts.par_windows(2)
            .map(|w| index_chunk(&map, w[0], w[1], &contains, &omits, key.as_ref()))
            .collect::<Result<_>>()
    })?;

    let kept: usize = chunks.iter().map(|c| c.offsets.len()).sum();
    if kept == 0 {
        bail!("no line of {} is kept", input.display());
    }
    if kept > u32::MAX as usize {
        bail!("{kept} lines is more than this tool indexes");
    }
    let mut offsets = Vec::with_capacity(kept);
    let mut lengths = Vec::with_capacity(kept);
    let mut values = Vec::with_capacity(if key.is_some() { kept } else { 0 });
    let mut names: Vec<Vec<u8>> = Vec::new();
    let mut global: HashMap<Vec<u8>, u32> = HashMap::new();
    let (mut checksum, mut lines, mut left_out) = (0u64, 0u64, 0u64);
    for chunk in &chunks {
        offsets.extend_from_slice(&chunk.offsets);
        lengths.extend_from_slice(&chunk.lengths);
        let remap: Vec<u32> = chunk.names.iter().map(|name| {
            let next = global.len() as u32;
            *global.entry(name.clone()).or_insert_with(|| {
                names.push(name.clone());
                next
            })
        }).collect();
        values.extend(chunk.values.iter().map(|&v| remap[v as usize]));
        checksum = checksum.wrapping_add(chunk.checksum);
        lines += chunk.lines;
        left_out += chunk.left_out;
    }
    drop(chunks);
    let seconds_index = started.elapsed().as_secs_f64();

    let started = Instant::now();
    let mut order: Vec<u32> = (0..kept as u32).collect();
    mt::Mt::new(options.seed).shuffle(&mut order);
    let seconds_shuffle = started.elapsed().as_secs_f64();

    let started = Instant::now();
    let _ = map.advise(Advice::Random);
    let total: u64 = lengths.iter().map(|&l| u64::from(l) + 1).sum();
    let partial = PathBuf::from(format!("{}.partial", output.display()));
    let out = OpenOptions::new().write(true).create_new(true).open(&partial)
        .with_context(|| format!("create {}", partial.display()))?;
    out.set_len(total)?;
    let cut = stretches(&order, &lengths, total, threads * 4);
    let written: Vec<(u64, u64, Vec<u64>, Vec<u64>)> = pool.install(|| {
        cut.par_iter().map(|s| -> Result<(u64, u64, Vec<u64>, Vec<u64>)> {
            let mut buffer = Vec::with_capacity(WRITE_BUFFER + (1 << 16));
            let mut at = s.offset;
            let (mut sum, mut rows) = (0u64, 0u64);
            let mut part_counts = vec![0u64; parts];
            let mut tally_counts = vec![0u64; if key.is_some() { names.len() * parts } else { 0 }];
            for (i, &row) in order[s.first..s.last].iter().enumerate() {
                let (pos, row) = (s.first + i, row as usize);
                let start = offsets[row] as usize;
                let line = &map[start..start + lengths[row] as usize];
                sum = sum.wrapping_add(xxh3_64(line));
                rows += 1;
                // Part k starts at floor(k * kept / parts), so two parts split at kept / 2,
                // the halves `--start 0 --limit kept/2` and `--start kept/2` give.
                let part = (((pos as u128 + 1) * parts as u128 - 1) / kept as u128) as usize;
                part_counts[part] += 1;
                if key.is_some() {
                    tally_counts[values[row] as usize * parts + part] += 1;
                }
                buffer.extend_from_slice(line);
                buffer.push(b'\n');
                if buffer.len() >= WRITE_BUFFER {
                    out.write_all_at(&buffer, at)?;
                    at += buffer.len() as u64;
                    buffer.clear();
                }
            }
            out.write_all_at(&buffer, at)?;
            Ok((sum, rows, part_counts, tally_counts))
        }).collect::<Result<_>>()
    })?;
    out.sync_all()?;
    let seconds_write = started.elapsed().as_secs_f64();

    let mut out_sum = 0u64;
    let mut out_rows = 0u64;
    let mut part_counts = vec![0u64; parts];
    let mut tally_counts = vec![0u64; if key.is_some() { names.len() * parts } else { 0 }];
    for (sum, rows, p, t) in &written {
        out_sum = out_sum.wrapping_add(*sum);
        out_rows += rows;
        part_counts.iter_mut().zip(p).for_each(|(a, b)| *a += b);
        tally_counts.iter_mut().zip(t).for_each(|(a, b)| *a += b);
    }
    if out_sum != checksum || out_rows != kept as u64 || fs::metadata(&partial)?.len() != total {
        bail!("{} does not hold every kept line once; it is left as it is", partial.display());
    }
    fs::rename(&partial, output)?;

    let tally = names.iter().enumerate()
        .map(|(i, name)| (String::from_utf8_lossy(name).into_owned(),
                          tally_counts[i * parts..(i + 1) * parts].to_vec()))
        .collect();
    Ok(Report { output: output.to_path_buf(), lines, kept: kept as u64, left_out, bytes: total,
                checksum, threads, parts: part_counts, tally, seconds_index, seconds_shuffle,
                seconds_write })
}
