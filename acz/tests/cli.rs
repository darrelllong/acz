// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! End-to-end tests of the `acz`, `unacz` and `aczcat` binaries.

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const ACZ: &str = env!("CARGO_BIN_EXE_acz");
const UNACZ: &str = env!("CARGO_BIN_EXE_unacz");
const ACZCAT: &str = env!("CARGO_BIN_EXE_aczcat");

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("acz-test-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn run(bin: &str, args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut sin = child.stdin.take().unwrap();
    let data = stdin.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = sin.write_all(&data);
    });
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    out
}

fn sample() -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..20_000u32 {
        v.extend_from_slice(format!("line {i}: {}\n", i.wrapping_mul(2_654_435_761) % 1000).as_bytes());
    }
    v
}

#[test]
fn pipeline_roundtrip_all_orders() {
    let data = sample();
    for order in ["-0", "-1", "-2"] {
        let c = run(ACZ, &[order], &data);
        assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
        assert!(c.stdout.len() < data.len() / 2);
        let d = run(ACZ, &["-d"], &c.stdout);
        assert!(d.status.success());
        assert_eq!(d.stdout, data);
        // `-` operand is stdin too.
        let d = run(UNACZ, &["-"], &c.stdout);
        assert_eq!(d.stdout, data);
    }
}

#[test]
fn dedup_option_roundtrips() {
    let mut data = sample();
    let copy = data.clone();
    data.extend_from_slice(&copy);
    for m in ["-2", "-3"] {
        let c = run(ACZ, &[m, "--dedup", "--block=64K", "-T3"], &data);
        assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
        let d = run(UNACZ, &[], &c.stdout);
        assert!(d.status.success());
        assert_eq!(d.stdout, data, "{m}");
    }
    let o = run(ACZ, &["--dedup=64K", "--block=64K"], b"");
    assert_eq!(o.status.code(), Some(1));
    // A one-byte window is not a window; it must not silently mean "off".
    let o = run(ACZ, &["--dedup=1"], b"");
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn output_does_not_depend_on_the_thread_count() {
    // About ten 64K blocks, the second half a copy of the first so that
    // --dedup has repeats that reach back across blocks.
    let mut data = sample();
    let copy = data.clone();
    data.extend_from_slice(&copy);
    for m in ["-0", "-1", "-2", "-3", "-4"] {
        for dedup in [None, Some("--dedup=1M")] {
            let pack = |t: &str| {
                // Keep the four-worker path independent of the host's auto budget.
                let mut args = vec![m, t, "--block=64K", "--memory=2G"];
                args.extend(dedup);
                let c = run(ACZ, &args, &data);
                assert!(c.status.success(), "{m} {t} {dedup:?}: {}", String::from_utf8_lossy(&c.stderr));
                c.stdout
            };
            let one = pack("-T1");
            assert_eq!(one, pack("-T4"), "{m} {dedup:?}");
            let decoded = run(UNACZ, &["-T4", "--memory=2G"], &one);
            assert!(decoded.status.success(), "{m} {dedup:?}: {}", String::from_utf8_lossy(&decoded.stderr));
            assert_eq!(decoded.stdout, data, "{m} {dedup:?}");
        }
    }
}

#[test]
fn empty_input() {
    let c = run(ACZ, &[], b"");
    assert!(c.status.success());
    let d = run(ACZCAT, &[], &c.stdout);
    assert!(d.status.success());
    assert!(d.stdout.is_empty());
}

#[test]
fn file_mode_keeps_source_and_restores() {
    let dir = tmpdir("files");
    let f = dir.join("data.txt");
    fs::write(&f, sample()).unwrap();
    let fs_ = f.to_str().unwrap();

    let o = run(ACZ, &[fs_], b"");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(o.stdout.is_empty());
    let z = dir.join("data.txt.acz");
    assert!(z.exists() && f.exists());

    // Refuses to overwrite without -f.
    let o = run(ACZ, &[fs_], b"");
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("already exists"));

    // Refuses to compress an .acz file again.
    let o = run(ACZ, &[z.to_str().unwrap()], b"");
    assert_eq!(o.status.code(), Some(1));

    // Decompress with --rm removes the .acz and restores the original.
    fs::remove_file(&f).unwrap();
    let o = run(UNACZ, &["--rm", z.to_str().unwrap()], b"");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!z.exists());
    assert_eq!(fs::read(&f).unwrap(), sample());

    // No stray temp files.
    let names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names.len(), 1, "{names:?}");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unknown_suffix_and_missing_file() {
    let dir = tmpdir("suffix");
    let f = dir.join("plain");
    fs::write(&f, b"x").unwrap();
    let o = run(UNACZ, &[f.to_str().unwrap()], b"");
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("unknown suffix"));

    // A missing file is reported, but later files are still processed.
    let g = dir.join("g");
    fs::write(&g, b"hello").unwrap();
    let o = run(ACZ, &[dir.join("nope").to_str().unwrap(), g.to_str().unwrap()], b"");
    assert_eq!(o.status.code(), Some(1));
    assert!(dir.join("g.acz").exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn stdout_concatenates_multiple_files() {
    let dir = tmpdir("concat");
    let a = dir.join("a");
    let b = dir.join("b");
    fs::write(&a, b"first\n").unwrap();
    fs::write(&b, b"second\n").unwrap();
    let o = run(ACZ, &["-c", a.to_str().unwrap(), b.to_str().unwrap()], b"");
    assert!(o.status.success());
    let d = run(ACZCAT, &[], &o.stdout);
    assert_eq!(d.stdout, b"first\nsecond\n");

    // -o writes everything to one file.
    let out = dir.join("both.acz");
    let o = run(ACZ, &["-o", out.to_str().unwrap(), a.to_str().unwrap(), "-"], b"third\n");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let d = run(ACZCAT, &[out.to_str().unwrap()], b"");
    assert_eq!(d.stdout, b"first\nthird\n");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn test_mode_and_corruption() {
    let data = sample();
    let c = run(ACZ, &[], &data).stdout;
    let t = run(ACZ, &["-t"], &c);
    assert!(t.status.success());
    assert!(t.stdout.is_empty());

    let mut bad = c.clone();
    let mid = bad.len() / 2;
    bad[mid] ^= 0x10;
    let t = run(ACZ, &["-t"], &bad);
    assert_eq!(t.status.code(), Some(1));

    let t = run(UNACZ, &[], b"definitely not compressed");
    assert_eq!(t.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&t.stderr).contains("not in acz format"));

    let t = run(UNACZ, &[], &c[..c.len() - 1]);
    assert_eq!(t.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&t.stderr).contains("truncated"));
}

#[test]
fn broken_pipe_exits_quietly() {
    // Decompress a large stream into a reader that stops after 1 byte.
    let big: Vec<u8> = (0..4_000_000u32).map(|i| (i % 251) as u8).collect();
    let c = run(ACZ, &[], &big).stdout;
    let mut child =
        Command::new(ACZCAT).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut sin = child.stdin.take().unwrap();
    let w = std::thread::spawn(move || {
        let _ = sin.write_all(&c);
    });
    let mut one = [0u8; 1];
    child.stdout.as_mut().unwrap().read_exact(&mut one).unwrap();
    drop(child.stdout.take());
    let mut err = String::new();
    child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    let status = child.wait().unwrap();
    w.join().unwrap();
    assert_eq!(status.code(), Some(141));
    assert!(err.is_empty(), "stderr: {err}");
}

#[test]
fn usage_errors() {
    let o = run(ACZ, &["--bogus"], b"");
    assert_eq!(o.status.code(), Some(1));
    let o = run(ACZ, &["-h"], b"");
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("Usage"));
}

/// Pipes deliver short reads: a writer that trickles 1–7 bytes at a time
/// with pauses, and a reader that drains stdout in small reads, must still
/// round-trip exactly (compress and decompress, all models).
#[test]
fn trickled_pipes_with_short_reads_and_writes() {
    use std::time::Duration;

    fn run_trickled(bin: &str, args: &[&str], input: Vec<u8>) -> (Vec<u8>, i32) {
        let mut child = Command::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut sin = child.stdin.take().unwrap();
        let writer = std::thread::spawn(move || {
            let mut i = 0;
            let mut k = 0usize;
            while i < input.len() {
                let n = (1 + k % 7).min(input.len() - i);
                sin.write_all(&input[i..i + n]).unwrap();
                sin.flush().unwrap();
                i += n;
                k += 1;
                if k % 97 == 0 {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        });
        let mut out = Vec::new();
        let mut sout = child.stdout.take().unwrap();
        let mut buf = [0u8; 5];
        let mut reads = 0usize;
        loop {
            let n = sout.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            reads += 1;
            if reads % 211 == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        writer.join().unwrap();
        let code = child.wait().unwrap().code().unwrap_or(-1);
        (out, code)
    }

    let data = sample();
    let data = &data[..30_000];
    for m in ["-0", "-1", "-2", "-3"] {
        let (packed, code) = run_trickled(ACZ, &[m, "-T3", "--block=64K"], data.to_vec());
        assert_eq!(code, 0, "{m} compress");
        // Byte-identical to a normal (non-trickled) run.
        assert_eq!(packed, run(ACZ, &[m, "-T3", "--block=64K"], data).stdout, "{m}");
        let (out, code) = run_trickled(UNACZ, &["-T2"], packed);
        assert_eq!(code, 0, "{m} decompress");
        assert_eq!(out, data, "{m}");
    }
}

/// Output safety: hard-linked and symlinked outputs, colliding temporary
/// names, destinations created during a run, and all-or-nothing `-o`
/// (Unix: hard links and FIFOs make the orderings deterministic).
#[cfg(unix)]
mod data_safety {
    use super::*;
    use std::fs::OpenOptions;

    fn mkfifo(p: &std::path::Path) {
        let st = Command::new("mkfifo").arg(p).status().unwrap();
        assert!(st.success());
    }

    #[test]
    fn output_hard_link_to_input_is_refused_and_nothing_is_lost() {
        let dir = tmpdir("hardlink");
        let input = dir.join("input");
        let other = dir.join("other");
        let alias = dir.join("alias");
        let data: Vec<u8> = (0..1900u32).map(|i| (i * 7) as u8).collect();
        fs::write(&input, &data).unwrap();
        fs::write(&other, b"second input").unwrap();
        fs::hard_link(&input, &alias).unwrap();
        for args in [
            vec!["-0", "-f", "-o", alias.to_str().unwrap(), input.to_str().unwrap()],
            vec!["-0", "-o", alias.to_str().unwrap(), input.to_str().unwrap()],
            vec!["-3", "-f", "-o", alias.to_str().unwrap(), other.to_str().unwrap(), input.to_str().unwrap()],
        ] {
            let o = run(ACZ, &args, b"");
            assert_eq!(o.status.code(), Some(1), "{args:?}");
            assert!(String::from_utf8_lossy(&o.stderr).contains("same file"), "{args:?}");
            assert_eq!(fs::read(&input).unwrap(), data);
            assert_eq!(fs::read(&alias).unwrap(), data);
        }
        // A symbolic link to the input is caught too.
        let sym = dir.join("sym");
        std::os::unix::fs::symlink(&input, &sym).unwrap();
        let o = run(ACZ, &["-f", "-o", sym.to_str().unwrap(), input.to_str().unwrap()], b"");
        assert_eq!(o.status.code(), Some(1));
        assert_eq!(fs::read(&input).unwrap(), data);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pre_existing_temp_name_is_never_deleted() {
        let dir = tmpdir("tmpowner");
        let feed = dir.join("feed");
        mkfifo(&feed);
        let child = Command::new(ACZ)
            .args(["-0", feed.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Plant a file at the first temporary name acz will try. acz cannot
        // create its temporary file until it has opened the FIFO, which
        // happens only once we open it for writing below.
        let sentinel = dir.join(format!(".feed.acz.{}.0.tmp", child.id()));
        fs::write(&sentinel, b"not yours").unwrap();
        fs::write(&feed, b"hello through a fifo").unwrap();
        let o = child.wait_with_output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(fs::read(&sentinel).unwrap(), b"not yours");
        let d = run(ACZCAT, &[dir.join("feed.acz").to_str().unwrap()], b"");
        assert_eq!(d.stdout, b"hello through a fifo");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn destination_created_during_run_is_not_overwritten() {
        let dir = tmpdir("noclobber");
        let feed = dir.join("feed");
        mkfifo(&feed);
        let child = Command::new(ACZ)
            .args(["-0", feed.to_str().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Opening the FIFO for writing blocks until acz opens it for
        // reading, i.e. until acz has passed its existence check.
        let mut w = OpenOptions::new().write(true).open(&feed).unwrap();
        // Now the destination appears, before acz publishes.
        let out = dir.join("feed.acz");
        fs::write(&out, b"precious").unwrap();
        w.write_all(b"data").unwrap();
        drop(w);
        let o = child.wait_with_output().unwrap();
        assert_eq!(o.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&o.stderr).contains("exists"));
        assert_eq!(fs::read(&out).unwrap(), b"precious");
        // No temporary files left behind.
        let names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn output_is_all_or_nothing_when_an_input_fails() {
        let dir = tmpdir("allornothing");
        let a = dir.join("a");
        fs::write(&a, b"good").unwrap();
        let out = dir.join("out.acz");
        let o =
            run(ACZ, &["-o", out.to_str().unwrap(), a.to_str().unwrap(), dir.join("missing").to_str().unwrap()], b"");
        assert_eq!(o.status.code(), Some(1));
        assert!(!out.exists());
        let names: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
