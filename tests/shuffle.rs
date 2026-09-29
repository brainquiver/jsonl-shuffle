//! What a shuffle can get wrong: an order that is not Python's for the same seed, a file that
//! changes with the number of threads, a line lost, doubled or cut, a filter that keeps the
//! wrong lines, and an existing file overwritten.

use std::fs;
use std::path::PathBuf;

use jsonl_shuffle::mt::Mt;
use jsonl_shuffle::{shuffle_file, Options};

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("jsonl-shuffle-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn options(seed: u64, threads: usize) -> Options {
    Options { seed, contains: vec![], omits: vec![], tally: None, parts: 2, threads }
}

fn lines(path: &PathBuf) -> Vec<String> {
    fs::read_to_string(path).unwrap().lines().map(str::to_string).collect()
}

#[test]
fn the_generator_is_cpythons() {
    // Reference values from Python 3.14: random.Random(seed).getrandbits(k).
    let mut mt = Mt::new(20_260_929);
    let got: Vec<u64> = (0..5).map(|_| mt.getrandbits(32)).collect();
    assert_eq!(got, [1643498841, 2223846887, 3159920272, 1248210412, 2858797283]);
    let mut mt = Mt::new(20_260_929);
    let got: Vec<u64> = (0..3).map(|_| mt.getrandbits(40)).collect();
    assert_eq!(got, [568579181913, 320987500176, 509664938211]);
    let mut mt = Mt::new(0);
    assert_eq!((0..3).map(|_| mt.getrandbits(32)).collect::<Vec<_>>(), [3626764237, 1654615998, 3255389356]);
    let mut mt = Mt::new((1 << 40) + 5);
    assert_eq!((0..3).map(|_| mt.getrandbits(32)).collect::<Vec<_>>(), [2166296868, 2220160828, 1153647273]);
}

#[test]
fn the_shuffle_is_cpythons() {
    // random.Random(20260929).shuffle(list(range(10)))
    let mut x: Vec<u32> = (0..10).collect();
    Mt::new(20_260_929).shuffle(&mut x);
    assert_eq!(x, [2, 7, 0, 9, 1, 3, 5, 4, 8, 6]);
    // random.Random(7).shuffle(list(range(100000))): the first eight, and a checksum of the rest.
    let mut x: Vec<u64> = (0..100_000).collect();
    Mt::new(7).shuffle(&mut x);
    assert_eq!(&x[..8], [26567, 77215, 84363, 16229, 78976, 54897, 62553, 20801]);
    let sum = x.iter().enumerate().fold(0u128, |a, (i, &v)| (a + i as u128 * v as u128) % 1_000_000_007);
    assert_eq!(sum, 644_543_935);
}

#[test]
fn every_kept_line_is_written_once_whatever_the_threads() {
    let d = dir("threads");
    let input = d.join("in.jsonl");
    let mut text = String::new();
    for i in 0..5000 {
        text.push_str(&format!("{{\"id\":{i},\"pad\":\"{}\"}}\n", "x".repeat(i % 97)));
        if i % 500 == 0 {
            text.push_str("\n   \n");
        }
    }
    text.push_str("{\"id\":\"last, with no line break\"}");
    fs::write(&input, &text).unwrap();
    let mut outputs = Vec::new();
    for threads in [1, 3, 14] {
        let out = d.join(format!("out{threads}.jsonl"));
        let report = shuffle_file(&input, &out, &options(42, threads)).unwrap();
        assert_eq!(report.kept, 5001);
        assert_eq!(report.left_out, 0);
        outputs.push(fs::read(&out).unwrap());
        let mut got = lines(&out);
        let mut want: Vec<String> = text.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect();
        assert_ne!(got, want, "the lines were not moved");
        got.sort();
        want.sort();
        assert_eq!(got, want);
        assert!(outputs.last().unwrap().ends_with(b"\n"));
    }
    assert!(outputs.windows(2).all(|w| w[0] == w[1]), "the file changed with the thread count");
    let other = d.join("seed.jsonl");
    shuffle_file(&input, &other, &options(43, 14)).unwrap();
    assert_ne!(fs::read(&other).unwrap(), outputs[0]);
}

#[test]
fn the_order_is_pythons_over_the_kept_lines() {
    let d = dir("python");
    let input = d.join("in.jsonl");
    let rows: Vec<String> = (0..10).map(|i| format!("{{\"n\":{i},\"prose\":{{}}}}")).collect();
    let mut text = String::new();
    for (i, row) in rows.iter().enumerate() {
        text.push_str(row);
        text.push('\n');
        text.push_str(&format!("{{\"n\":\"structured {i}\"}}\n"));
    }
    fs::write(&input, text).unwrap();
    let out = d.join("out.jsonl");
    let mut opt = options(20_260_929, 4);
    opt.contains = vec![b"\"prose\":{".to_vec()];
    let report = shuffle_file(&input, &out, &opt).unwrap();
    assert_eq!((report.kept, report.left_out, report.lines), (10, 10, 20));
    // random.Random(20260929).shuffle(list(range(10))) is [2, 7, 0, 9, 1, 3, 5, 4, 8, 6].
    let want: Vec<String> = [2, 7, 0, 9, 1, 3, 5, 4, 8, 6].iter().map(|&i| rows[i].clone()).collect();
    assert_eq!(lines(&out), want);
}

#[test]
fn filters_keep_and_leave_out() {
    let d = dir("filters");
    let input = d.join("in.jsonl");
    fs::write(&input, "{\"a\":1,\"b\":1}\n{\"a\":1}\n{\"b\":1}\n{\"a\":1,\"b\":1,\"c\":1}\n").unwrap();
    let out = d.join("out.jsonl");
    let mut opt = options(1, 2);
    opt.contains = vec![b"\"a\"".to_vec(), b"\"b\"".to_vec()];
    opt.omits = vec![b"\"c\"".to_vec()];
    let report = shuffle_file(&input, &out, &opt).unwrap();
    assert_eq!((report.kept, report.left_out), (1, 3));
    assert_eq!(lines(&out), ["{\"a\":1,\"b\":1}"]);
}

#[test]
fn an_existing_output_is_refused() {
    let d = dir("refuse");
    let input = d.join("in.jsonl");
    fs::write(&input, "{\"a\":1}\n{\"a\":2}\n").unwrap();
    let out = d.join("out.jsonl");
    fs::write(&out, "keep me").unwrap();
    assert!(shuffle_file(&input, &out, &options(1, 2)).is_err());
    assert_eq!(fs::read_to_string(&out).unwrap(), "keep me");
}

#[test]
fn a_field_is_counted_in_each_part() {
    let d = dir("tally");
    let input = d.join("in.jsonl");
    let mut text = String::new();
    for i in 0..1000 {
        let workflow = ["alpha", "beta", "gamma"][i % 3];
        text.push_str(&format!("{{\"metadata\":{{\"model\":null}},\"id\":{i},\"workflow\": \"{workflow}\"}}\n"));
    }
    fs::write(&input, text).unwrap();
    let out = d.join("out.jsonl");
    let mut opt = options(9, 5);
    opt.tally = Some("workflow".into());
    let report = shuffle_file(&input, &out, &opt).unwrap();
    assert_eq!(report.parts, [500, 500]);
    let names: Vec<&str> = report.tally.iter().map(|(v, _)| v.as_str()).collect();
    assert_eq!(names, ["alpha", "beta", "gamma"]);
    let totals: Vec<u64> = report.tally.iter().map(|(_, c)| c.iter().sum()).collect();
    assert_eq!(totals, [334, 333, 333]);
}

#[test]
fn two_parts_split_where_integer_halves_do() {
    // Python's halves of 3999999 rows are 1999999 and 2000000: the first part is n / 2.
    let d = dir("halves");
    let input = d.join("in.jsonl");
    fs::write(&input, (0..7).map(|i| format!("{{\"i\":{i}}}\n")).collect::<String>()).unwrap();
    let report = shuffle_file(&input, &d.join("out.jsonl"), &options(1, 3)).unwrap();
    assert_eq!(report.parts, [3, 4]);
    let mut opt = options(1, 3);
    opt.parts = 3;
    let report = shuffle_file(&input, &d.join("out3.jsonl"), &opt).unwrap();
    assert_eq!(report.parts, [2, 2, 3]);
}
