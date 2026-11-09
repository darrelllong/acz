// Copyright (c) 2026 Darrell D. E. Long. All rights reserved.
// Use and redistribution are subject to the BSD 3-clause license, with a
// dedication clause, in the LICENSE file at the root of this repository.
//
// Dedicated to the memory of my colleague Glen G. Langdon

//! Command-line front end shared by the `acz`, `unacz` and `aczcat` binaries.
//!
//! Semantics follow `gzip`/`zstd`:
//!
//! * with no file operands, or the operand `-`, data flows stdin → stdout,
//!   so the tool is a transducer in a pipeline;
//! * `acz FILE` writes `FILE.acz`; `acz -d FILE.acz` writes `FILE`.
//!   Sources are kept unless `--rm` is given (as in zstd);
//! * `-c` sends everything to stdout; `-o OUT` sends everything to `OUT`.
//!   Multiple inputs are concatenated into one multi-member stream;
//! * compressed data is never written to, or read from, a terminal unless
//!   `-f` is given;
//! * if stdout is closed early (`acz -dc x.acz | head`) the tool stops
//!   quietly with status 141, as if killed by SIGPIPE.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::format::{self, Stats};
use crate::models::Order;

/// File name suffix for compressed files.
pub const SUFFIX: &str = ".acz";
const BUF: usize = 64 * 1024;
/// Exit status used when stdout is a broken pipe (128 + SIGPIPE).
const EXIT_PIPE: u8 = 141;

/// What to do with each input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Compress.
    Compress,
    /// Decompress.
    Decompress,
    /// Decompress and verify, discarding the output.
    Test,
}

#[derive(Debug)]
struct Opts {
    mode: Mode,
    to_stdout: bool,
    output: Option<PathBuf>,
    force: bool,
    remove: bool,
    verbose: bool,
    quiet: bool,
    /// Coding settings; decompression uses its threads and memory.
    enc: format::Options,
    files: Vec<OsString>,
}

enum Parsed {
    Run(Opts),
    Help,
    Version,
}

fn usage(prog: &str) -> String {
    format!(
        "\
Usage: {prog} [OPTION]... [FILE]...
Compress or decompress FILEs with an adaptive arithmetic coder.
With no FILE, or when FILE is -, read standard input and write standard output.

  -z, --compress      compress (default for acz)
  -d, --decompress    decompress (default for unacz, aczcat);
                        also --uncompress
  -t, --test          test compressed file integrity
  -c, --stdout        write to standard output, keep input files;
                        also --to-stdout
  -o, --output=FILE   write all output to FILE
  -f, --force         overwrite existing outputs; allow terminal I/O
  -k, --keep          keep input files (default)
      --rm            remove input files after success
  -0 .. -4            model (default -4); also --order=N:
                        0 = memoryless, 1 = previous byte,
                        2 = previous two bytes, binary-decomposed,
                        3 = prediction by partial matching (PPM),
                        4 = context mixing (best compression)
  -T, --threads=N     worker threads (default, or 0: all available
                        cores)
      --block=SIZE    block size from 64K to 1G (default 16M; also
                        --block-size); larger compresses better,
                        smaller parallelises better
      --dedup[=SIZE]  replace long repeats (256 bytes or more) found up
                        to SIZE back (default 256M, at most 64G, larger
                        than the block) by copy commands
      --no-dedup      do not (the default; the last of the two wins)
      --memory=SIZE   memory limit, or 'auto' (the default): half of
                        physical memory, or 4G if that is unknown; fewer
                        threads are used if needed, and a stream that
                        cannot fit is refused before anything is allocated
  -v, --verbose       report compression statistics on stderr
  -q, --quiet         suppress warnings
  -h, --help          show this help
  -V, --version       show version

SIZE is a number of bytes, optionally followed by K, M or G (powers of 1024;
16M, 16MB and 16MiB are the same, in either case). Block and window sizes are rounded up
to a power of two.
A long option's value may follow '=' or be the next argument, except
--dedup's, which must follow '='.
Decompression needs no options: the model, block size and window are read
from the stream.

Compressed files get the suffix {SUFFIX}. Concatenated {SUFFIX} streams
decompress to the concatenation of their contents.
Exit status: 0 success, 1 error, 141 output pipe closed.
"
    )
}

fn parse_threads(v: &str) -> Result<usize, String> {
    match v.parse::<usize>() {
        Ok(0) => Ok(format::default_threads()),
        Ok(n) => Ok(n.min(format::MAX_THREADS)),
        Err(_) => Err(format!("invalid thread count '{v}'")),
    }
}

/// Parse a byte count: `N`, optionally followed by `K`, `M` or `G` (powers
/// of 1024), each optionally followed by `B` or `iB`, in any case (`16M`,
/// `16mb`, `16MiB`).
fn parse_size(v: &str) -> Option<u64> {
    let lower = v.to_ascii_lowercase();
    let s = lower.strip_suffix('b').unwrap_or(&lower);
    // "iB" belongs to a multiple ("MiB"); a plain "B" may follow any count.
    let (s, binary) = match s.strip_suffix('i') {
        Some(s) => (s, true),
        None => (s, false),
    };
    let (num, mult) = match s.as_bytes().last() {
        Some(b'k') => (&s[..s.len() - 1], 1u64 << 10),
        Some(b'm') => (&s[..s.len() - 1], 1u64 << 20),
        Some(b'g') => (&s[..s.len() - 1], 1u64 << 30),
        _ if !binary => (s, 1),
        _ => return None,
    };
    num.parse::<u64>().ok().and_then(|n| n.checked_mul(mult))
}

/// A power-of-two size the way a user writes it (`64K`, `16M`, `1G`).
fn show_size(n: u64) -> String {
    match n.trailing_zeros() {
        30.. => format!("{}G", n >> 30),
        20.. => format!("{}M", n >> 20),
        10.. => format!("{}K", n >> 10),
        _ => n.to_string(),
    }
}

/// log2 of a size, rounded up to a power of two (so the result is never
/// smaller than asked for).
fn parse_log2(v: &str) -> Option<u8> {
    parse_size(v).filter(|&n| n > 0).and_then(u64::checked_next_power_of_two).map(|n| n.trailing_zeros() as u8)
}

fn parse_block(v: &str) -> Result<u8, String> {
    parse_log2(v)
        .filter(|l| (format::MIN_BLOCK_LOG2..=format::MAX_BLOCK_LOG2).contains(l))
        .ok_or_else(|| format!("invalid block size '{v}' (64K to 1G)"))
}

fn parse_dedup(v: &str) -> Result<u8, String> {
    parse_log2(v)
        .filter(|l| (1..=format::MAX_DEDUP_LOG2).contains(l))
        .ok_or_else(|| format!("invalid dedup window '{v}' (2 to 64G)"))
}

/// Set the option for a long flag that takes no value; false if `name` is
/// not one.
fn set_flag(o: &mut Opts, name: &str) -> bool {
    match name {
        "compress" => o.mode = Mode::Compress,
        "decompress" | "uncompress" => o.mode = Mode::Decompress,
        "test" => o.mode = Mode::Test,
        "stdout" | "to-stdout" => o.to_stdout = true,
        "force" => o.force = true,
        "keep" => o.remove = false,
        "rm" => o.remove = true,
        "verbose" => o.verbose = true,
        "quiet" => o.quiet = true,
        "no-dedup" => o.enc.dedup_log2 = 0,
        _ => return false,
    }
    true
}

/// The value of a long option: after `=` (`--threads=4`), else the next
/// argument (`--threads 4`).
fn long_value(val: Option<String>, it: &mut impl Iterator<Item = OsString>, name: &str) -> Result<OsString, String> {
    match val {
        Some(v) => Ok(OsString::from(v)),
        None => it.next().ok_or_else(|| format!("option '--{name}' requires an argument")),
    }
}

/// The value of a short option such as `-T`: the rest of the argument
/// (`-T4`), else the next argument (`-T 4`).
fn short_value(rest: &str, it: &mut impl Iterator<Item = OsString>, flag: char) -> Result<OsString, String> {
    if rest.is_empty() {
        it.next().ok_or_else(|| format!("option '-{flag}' requires an argument"))
    } else {
        Ok(OsString::from(rest))
    }
}

/// Parse flags in order, retaining file operands as OS strings so paths
/// need not be UTF-8. `--` ends option parsing; a lone `-` remains stdin.
/// Value-taking short options consume the remainder of their cluster or
/// the next argument. Dependent options are checked after the scan, so
/// their order on the command line does not affect block/window validation.
fn parse(args: Vec<OsString>, default_mode: Mode, default_stdout: bool) -> Result<Parsed, String> {
    let mut o = Opts {
        mode: default_mode,
        to_stdout: default_stdout,
        output: None,
        force: false,
        remove: false,
        verbose: false,
        quiet: false,
        enc: format::Options::default(),
        files: Vec::new(),
    };
    let mut it = args.into_iter();
    let mut only_files = false;
    // Sizes as given, before rounding, to tell whether a window was asked
    // to be larger than the block.
    let mut window_asked: Option<u64> = None;
    let mut block_asked = 1u64 << o.enc.block_log2;
    let mut memory_auto = true;
    while let Some(arg) = it.next() {
        if only_files || arg == "-" || !arg.to_string_lossy().starts_with('-') {
            o.files.push(arg);
            continue;
        }
        let s = arg.to_str().ok_or_else(|| format!("invalid option {:?}", arg))?;
        if s == "--" {
            only_files = true;
        } else if let Some(long) = s.strip_prefix("--") {
            let (name, val) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            if set_flag(&mut o, name) {
                if val.is_some() {
                    return Err(format!("option '--{name}' takes no argument"));
                }
                continue;
            }
            match name {
                "help" => return Ok(Parsed::Help),
                "version" => return Ok(Parsed::Version),
                "output" => o.output = Some(PathBuf::from(long_value(val, &mut it, name)?)),
                "threads" => o.enc.threads = parse_threads(&long_value(val, &mut it, name)?.to_string_lossy())?,
                "dedup" => {
                    o.enc.dedup_log2 = match &val {
                        None => format::DEFAULT_DEDUP_LOG2,
                        Some(v) => parse_dedup(v)?,
                    };
                    window_asked = val.as_deref().and_then(parse_size);
                }
                "memory" => {
                    let v = long_value(val, &mut it, name)?.to_string_lossy().into_owned();
                    memory_auto = v.eq_ignore_ascii_case("auto");
                    if !memory_auto {
                        o.enc.memory = parse_size(&v).ok_or_else(|| format!("invalid memory size '{v}'"))?;
                    }
                }
                "block" | "block-size" => {
                    let v = long_value(val, &mut it, name)?.to_string_lossy().into_owned();
                    o.enc.block_log2 = parse_block(&v)?;
                    block_asked = parse_size(&v).unwrap_or(0);
                }
                "order" => {
                    let v = long_value(val, &mut it, name)?.to_string_lossy().into_owned();
                    o.enc.order = match v.as_str() {
                        "0" => Order::Zero,
                        "1" => Order::One,
                        "2" => Order::Two,
                        "3" => Order::Ppm,
                        "4" => Order::Mix,
                        _ => return Err(format!("invalid order '{v}' (expected 0..4)")),
                    };
                }
                _ => return Err(format!("unrecognized option '--{name}'")),
            }
        } else {
            for (i, c) in s[1..].char_indices() {
                match c {
                    'z' => o.mode = Mode::Compress,
                    'd' => o.mode = Mode::Decompress,
                    't' => o.mode = Mode::Test,
                    'c' => o.to_stdout = true,
                    'f' => o.force = true,
                    'k' => o.remove = false,
                    'v' => o.verbose = true,
                    'q' => o.quiet = true,
                    '0' => o.enc.order = Order::Zero,
                    '1' => o.enc.order = Order::One,
                    '2' => o.enc.order = Order::Two,
                    '3' => o.enc.order = Order::Ppm,
                    '4' => o.enc.order = Order::Mix,
                    'h' => return Ok(Parsed::Help),
                    'V' => return Ok(Parsed::Version),
                    // These take the rest of the argument as their value.
                    'T' => {
                        let v = short_value(&s[2 + i..], &mut it, c)?;
                        o.enc.threads = parse_threads(&v.to_string_lossy())?;
                        break;
                    }
                    'o' => {
                        o.output = Some(PathBuf::from(short_value(&s[2 + i..], &mut it, c)?));
                        break;
                    }
                    _ => return Err(format!("invalid option -- '{c}'")),
                }
            }
        }
    }
    if o.files.is_empty() {
        o.files.push(OsString::from("-"));
    }
    let (w, b) = (o.enc.dedup_log2, o.enc.block_log2);
    if w != 0 && w <= b {
        // Rounding both up can make a window that was asked to be larger
        // than the block equal to it; then it takes the next size up.
        if window_asked.is_some_and(|wa| wa > block_asked) && b < format::MAX_DEDUP_LOG2 {
            o.enc.dedup_log2 = b + 1;
        } else {
            return Err(format!(
                "the --dedup window ({}) must be larger than the block ({})",
                show_size(1 << w),
                show_size(1 << b)
            ));
        }
    }
    if o.to_stdout && o.output.is_some() {
        return Err("options -c and -o are mutually exclusive".into());
    }
    if memory_auto {
        o.enc.memory = auto_memory();
    }
    Ok(Parsed::Run(o))
}

/// The default memory budget: half of physical memory (or of a smaller
/// cgroup limit), leaving the rest of the machine for everything else, or
/// [`format::DEFAULT_MEMORY`] where neither can be found. The budget only
/// limits how many workers run, so it never changes the output.
fn auto_memory() -> u64 {
    let phys = physical_memory();
    let limit = cgroup_limit();
    match (phys, limit) {
        (Some(p), Some(l)) => p.min(l) / 2,
        (Some(m), None) | (None, Some(m)) => m / 2,
        (None, None) => format::DEFAULT_MEMORY,
    }
}

/// Physical memory in bytes. Without `unsafe` there is no portable call, so
/// this reads `/proc/meminfo` on Linux and asks `sysctl` (by absolute path)
/// on macOS and the BSDs.
fn physical_memory() -> Option<u64> {
    if let Ok(s) = fs::read_to_string("/proc/meminfo") {
        let kb = s.lines().find_map(|l| l.strip_prefix("MemTotal:"))?;
        return kb.trim().strip_suffix("kB")?.trim().parse::<u64>().ok()?.checked_mul(1024);
    }
    let (sysctl, key) = if cfg!(target_os = "macos") {
        ("/usr/sbin/sysctl", "hw.memsize")
    } else if cfg!(any(target_os = "freebsd", target_os = "dragonfly")) {
        ("/sbin/sysctl", "hw.physmem")
    } else if cfg!(any(target_os = "openbsd", target_os = "netbsd")) {
        ("/sbin/sysctl", "hw.physmem64")
    } else {
        return None;
    };
    let out = std::process::Command::new(sysctl).args(["-n", key]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

/// The tightest cgroup v2 memory limit on this process, if any. In a
/// container this can be far below physical memory.
fn cgroup_limit() -> Option<u64> {
    let own = fs::read_to_string("/proc/self/cgroup").ok()?;
    let mounts = fs::read_to_string("/proc/self/mountinfo").ok()?;
    cgroup_limit_in(&own, &mounts)
}

/// The smallest `memory.max` of this process's cgroup and of each ancestor
/// visible through the cgroup2 mounts. `self_cgroup` is the contents of
/// `/proc/self/cgroup` and `mountinfo` of `/proc/self/mountinfo`.
///
/// The membership line `0::/path` names the cgroup from the root of the
/// cgroup hierarchy, but the mount may expose only a subtree (its root,
/// field 4 of mountinfo) at any mountpoint (field 5). So the path is taken
/// relative to the mount's root and joined to its mountpoint, and the walk
/// upward stops at the mountpoint. A limit set on any ancestor binds the
/// process too. All matching mounts are read: a subtree bind mount can
/// hide an ancestor that another mount exposes. "max" (no limit) and
/// missing files are skipped.
fn cgroup_limit_in(self_cgroup: &str, mountinfo: &str) -> Option<u64> {
    let path = self_cgroup.lines().find_map(|l| l.strip_prefix("0::"))?;
    let mounts = mountinfo.lines().filter_map(|line| {
        let (fields, rest) = line.split_once(" - ")?;
        if rest.split(' ').next()? != "cgroup2" {
            return None;
        }
        let mut f = fields.split(' ');
        let root = unescape_mount(f.nth(3)?);
        let mnt = PathBuf::from(unescape_mount(f.next()?));
        let rel = if root == "/" {
            path.strip_prefix('/')?
        } else if path == root {
            ""
        } else {
            path.strip_prefix(root.as_str())?.strip_prefix('/')?
        };
        // A cgroup outside the namespace can appear with "..". It
        // cannot be reached through this mount without escaping it.
        if Path::new(rel).components().any(|c| c == std::path::Component::ParentDir) {
            return None;
        }
        Some((rel.to_string(), mnt))
    });
    let mut limit: Option<u64> = None;
    for (rel, mnt) in mounts {
        let mut dir = mnt.join(&rel);
        loop {
            let v = fs::read_to_string(dir.join("memory.max")).ok().and_then(|s| s.trim().parse::<u64>().ok());
            if let Some(v) = v {
                limit = Some(limit.map_or(v, |l| l.min(v)));
            }
            if dir == mnt || !dir.pop() {
                break;
            }
        }
    }
    limit
}

/// Undo mountinfo's escapes: space, tab, newline and backslash appear as
/// `\040`, `\011`, `\012` and `\134`.
fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let code = b.get(i + 1..i + 4).filter(|d| b[i] == b'\\' && d.iter().all(|c| (b'0'..=b'7').contains(c)));
        if let Some(d) = code.and_then(|d| u8::try_from(d.iter().fold(0u32, |v, c| v * 8 + u32::from(c - b'0'))).ok()) {
            out.push(d);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Error from processing one input.
enum Fail {
    /// Report and continue with the next file.
    Msg(String),
    /// Stdout is closed; stop everything quietly.
    BrokenPipe,
}

impl From<String> for Fail {
    fn from(s: String) -> Self {
        Fail::Msg(s)
    }
}

fn io_fail(context: &str, e: io::Error) -> Fail {
    if e.kind() == io::ErrorKind::BrokenPipe {
        Fail::BrokenPipe
    } else {
        Fail::Msg(format!("{context}: {e}"))
    }
}

fn run_stream(opts: &Opts, input: &mut dyn io::BufRead, output: &mut dyn Write) -> io::Result<Stats> {
    let e = &opts.enc;
    match opts.mode {
        Mode::Compress => format::compress(input, output, e),
        Mode::Decompress | Mode::Test => {
            format::decompress(input, output, &format::DecodeOptions { threads: e.threads, memory: e.memory })
        }
    }
}

/// The error for an output that exists and may not be replaced.
fn refuse_existing(out: &Path) -> String {
    format!("{}: already exists (use -f to overwrite)", out.display())
}

/// A failed publication, with a hint when the output appeared meanwhile.
fn publish_error(out: &Path, e: &io::Error) -> String {
    let hint = if e.kind() == io::ErrorKind::AlreadyExists { " (use -f to overwrite)" } else { "" };
    format!("{}: {e}{hint}", out.display())
}

fn report(opts: &Opts, name: &str, st: Stats, dest: Option<&Path>) {
    if !opts.verbose {
        return;
    }
    if opts.mode == Mode::Test {
        eprintln!("{name}: OK ({} bytes)", st.raw);
        return;
    }
    let ratio = if st.raw == 0 { 0.0 } else { st.packed as f64 / st.raw as f64 };
    let arrow = dest.map(|d| format!(" -> {}", d.display())).unwrap_or_default();
    let (from, to) = if opts.mode == Mode::Compress { (st.raw, st.packed) } else { (st.packed, st.raw) };
    eprintln!("{name}: {from} -> {to} bytes, {:.2}%, {:.3} bits/byte{arrow}", 100.0 * ratio, 8.0 * ratio);
}

/// Name of the per-file output for `input`.
fn derived_output(opts: &Opts, input: &Path) -> Result<PathBuf, String> {
    let s = input.as_os_str();
    let has_suffix = s.as_encoded_bytes().ends_with(SUFFIX.as_bytes());
    match opts.mode {
        Mode::Compress => {
            if has_suffix && !opts.force {
                return Err(format!("{}: already has {SUFFIX} suffix -- unchanged", input.display()));
            }
            let mut o = s.to_os_string();
            o.push(SUFFIX);
            Ok(PathBuf::from(o))
        }
        Mode::Decompress | Mode::Test => {
            let name = input.file_name().unwrap_or(OsStr::new(""));
            if !has_suffix || name.len() <= SUFFIX.len() {
                return Err(format!("{}: unknown suffix -- ignored", input.display()));
            }
            // The file name ends in ".acz" and has a non-empty stem, so the
            // extension is exactly "acz".
            Ok(input.with_extension(""))
        }
    }
}

/// A file written under a temporary name beside its destination. The
/// destination is published only after coding and file synchronization
/// succeed, so a codec failure cannot replace it with partial output.
/// Keeping both names in one directory permits an atomic link or rename.
///
/// `owns_tmp` records which name Drop may remove. A colliding temporary
/// name is never opened or deleted; an abruptly killed process may leave
/// its own temporary file behind.
struct Staged {
    tmp: PathBuf,
    out: PathBuf,
    writer: Option<BufWriter<File>>,
    /// True while `tmp` is a file this process created and still owns.
    owns_tmp: bool,
}

impl Staged {
    /// Claim a new temporary name atomically. A preliminary existence
    /// check would race another process; `create_new` makes collision
    /// detection part of creation. Only successful creation gives this
    /// object permission to remove the temporary name on failure.
    fn create(out: &Path) -> io::Result<Self> {
        let base = out.file_name().unwrap_or(OsStr::new("out"));
        for attempt in 0..100u32 {
            let mut name = OsString::from(".");
            name.push(base);
            name.push(format!(".{}.{attempt}.tmp", std::process::id()));
            let tmp = out.with_file_name(name);
            match OpenOptions::new().write(true).create_new(true).open(&tmp) {
                Ok(f) => {
                    return Ok(Staged {
                        tmp,
                        out: out.to_path_buf(),
                        writer: Some(BufWriter::with_capacity(BUF, f)),
                        owns_tmp: true,
                    })
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(io::ErrorKind::AlreadyExists, "could not create a temporary file"))
    }

    fn writer(&mut self) -> &mut BufWriter<File> {
        self.writer.as_mut().expect("staged file is open until published")
    }

    /// Publish the completed file. First flush buffered bytes, copy
    /// permissions and mtime when available, and sync the file itself.
    /// Then use rename for explicit replacement or hard_link to create
    /// a destination that must not exist. The latter is an atomic check:
    /// an earlier path-existence check alone cannot prevent overwrites.
    ///
    /// Remove the temporary name after linking, then sync the directory.
    /// A failure after publication can leave a complete output in place;
    /// report that failure so the caller retains its source. There is no
    /// fallback to a non-atomic copy when linking is unavailable.
    fn publish(mut self, force: bool, meta: Option<&fs::Metadata>) -> io::Result<()> {
        let w = self.writer.take().expect("published once");
        let f = w.into_inner().map_err(|e| e.into_error())?;
        if let Some(meta) = meta {
            let _ = f.set_permissions(meta.permissions());
            if let Ok(mtime) = meta.modified() {
                let _ = f.set_modified(mtime);
            }
        }
        f.sync_all()?;
        drop(f);
        if force {
            fs::rename(&self.tmp, &self.out)?;
            self.owns_tmp = false;
        } else {
            fs::hard_link(&self.tmp, &self.out)
                .map_err(|e| io::Error::new(e.kind(), format!("cannot publish output without overwriting: {e}")))?;
            // The output is in place; remove our temporary name now rather
            // than leaving it to Drop. If this fails, Drop tries again.
            fs::remove_file(&self.tmp).map_err(|e| {
                io::Error::new(e.kind(), format!("output written, but its temporary name remains: {e}"))
            })?;
            self.owns_tmp = false;
        }
        // Make the new directory entry durable.
        sync_parent(&self.out).map_err(|e| {
            io::Error::new(e.kind(), format!("output written, but its directory could not be synced: {e}"))
        })
    }
}

/// Sync the directory holding `path` to persist a new or renamed entry.
/// Unix only; elsewhere this does nothing.
fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d,
            _ => Path::new("."),
        };
        File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        // Close the file first, then remove the temporary name, but only
        // if it is still ours (not renamed into place).
        self.writer = None;
        if self.owns_tmp {
            let _ = fs::remove_file(&self.tmp);
        }
    }
}

/// Process one input. `shared` is the sink every output goes to (stdout,
/// the `-o` file, or a discarding sink for `-t`); `None` means one output
/// per input, named after it.
///
/// Shared sinks are owned and finalized by `main`; this function must not
/// publish them or remove their inputs. Per-file output is staged here,
/// published after a successful stream operation, and only then may
/// `--rm` remove that input. Errors before publication drop the temporary
/// file, while errors after publication preserve the source.
fn process(opts: &Opts, name: &OsStr, shared: Option<&mut dyn Write>) -> Result<(), Fail> {
    let display = name.to_string_lossy().into_owned();

    if name == "-" {
        let stdin = io::stdin();
        if opts.mode != Mode::Compress && stdin.is_terminal() && !opts.force {
            return Err(Fail::Msg("refusing to read compressed data from a terminal (use -f to force)".into()));
        }
        let mut input = BufReader::with_capacity(BUF, stdin.lock());
        let w = shared.expect("stdin always has a shared sink");
        let st = run_stream(opts, &mut input, w).map_err(|e| io_fail("stdin", e))?;
        report(opts, "stdin", st, None);
        return Ok(());
    }

    let path = Path::new(name);
    let meta = fs::metadata(path).map_err(|e| format!("{display}: {e}"))?;
    if meta.is_dir() {
        return Err(Fail::Msg(format!("{display}: is a directory -- ignored")));
    }
    // Per-file mode: fail fast, before opening (and possibly blocking on)
    // the input, if the output already exists. Publication re-checks
    // atomically, so an output appearing later is still never replaced.
    let per_file_out = if shared.is_none() {
        let out = derived_output(opts, path)?;
        if !opts.force && fs::symlink_metadata(&out).is_ok() {
            return Err(Fail::Msg(refuse_existing(&out)));
        }
        Some(out)
    } else {
        None
    };
    let file = File::open(path).map_err(|e| format!("{display}: {e}"))?;
    let mut input = BufReader::with_capacity(BUF, file);

    let Some(out) = per_file_out else {
        let w = shared.expect("a shared sink when outputs are not per file");
        let st = run_stream(opts, &mut input, w).map_err(|e| io_fail(&display, e))?;
        report(opts, &display, st, None);
        return Ok(());
    };
    let mut staged = Staged::create(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let st = run_stream(opts, &mut input, staged.writer()).map_err(|e| io_fail(&display, e))?;
    staged.publish(opts.force, Some(&meta)).map_err(|e| publish_error(&out, &e))?;
    report(opts, &display, st, Some(&out));
    if opts.remove {
        fs::remove_file(path).map_err(|e| format!("{display}: cannot remove: {e}"))?;
    }
    Ok(())
}

/// Whether `a` and `b` name the same file: the same canonical path, or (on
/// Unix) the same device and inode, which also catches hard links.
fn same_file(a: &Path, b: &Path) -> bool {
    if let (Ok(x), Ok(y)) = (fs::canonicalize(a), fs::canonicalize(b)) {
        if x == y {
            return true;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(x), Ok(y)) = (fs::metadata(a), fs::metadata(b)) {
            return x.dev() == y.dev() && x.ino() == y.ino();
        }
    }
    false
}

/// Entry point. `default_mode` / `default_stdout` select the personality
/// (`acz`, `unacz`, `aczcat`).
///
/// Select one sink per operand: a discard sink for testing, a shared
/// staged file for `-o`, stdout, or a separate staged output. Per-file
/// errors are reported while processing continues; a closed stdout ends
/// the run immediately. Shared `-o` output is published only if all inputs
/// and the final flush succeeded, so one failure discards the whole result.
pub fn main(default_mode: Mode, default_stdout: bool) -> ExitCode {
    let mut args = std::env::args_os();
    let prog = args
        .next()
        .and_then(|a| Path::new(&a).file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "acz".into());

    let opts = match parse(args.collect(), default_mode, default_stdout) {
        Ok(Parsed::Run(o)) => o,
        Ok(Parsed::Help) => {
            print!("{}", usage(&prog));
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Version) => {
            println!("{prog} {}", env!("CARGO_PKG_VERSION"));
            println!("Dedicated to the memory of my colleague Glen G. Langdon");
            println!("BSD 3-clause license with a dedication clause; see LICENSE.");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("{prog}: {e}\nTry '{prog} --help' for more information.");
            return ExitCode::FAILURE;
        }
    };

    let mut failed = false;
    let complain = |msg: &str| eprintln!("{prog}: {msg}");

    let uses_stdout =
        opts.mode != Mode::Test && opts.output.is_none() && (opts.to_stdout || opts.files.iter().any(|f| f == "-"));
    let stdout = io::stdout();
    if uses_stdout && opts.mode == Mode::Compress && stdout.is_terminal() && !opts.force {
        complain("refusing to write compressed data to a terminal (use -f to force)");
        return ExitCode::FAILURE;
    }
    if opts.remove && (opts.to_stdout || opts.output.is_some() || opts.mode == Mode::Test) && !opts.quiet {
        complain("warning: --rm ignored with -c, -o or -t");
    }

    // Claim the shared output before reading inputs. Refuse aliases of
    // any input, including hard links, so publication cannot replace one
    // of the files that supplied this multi-member stream.
    let mut out_file: Option<Staged> = None;
    if let Some(out) = &opts.output {
        if opts.mode != Mode::Test {
            if let Some(f) = opts.files.iter().find(|f| *f != "-" && same_file(Path::new(f), out)) {
                complain(&format!("{}: input and output are the same file", Path::new(f).display()));
                return ExitCode::FAILURE;
            }
            if !opts.force && fs::symlink_metadata(out).is_ok() {
                complain(&refuse_existing(out));
                return ExitCode::FAILURE;
            }
            match Staged::create(out) {
                Ok(s) => out_file = Some(s),
                Err(e) => {
                    complain(&format!("{}: {e}", out.display()));
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    let mut stdout_w = BufWriter::with_capacity(BUF, stdout.lock());
    let mut discard = io::sink();

    for name in &opts.files {
        let shared: Option<&mut dyn Write> = if opts.mode == Mode::Test {
            Some(&mut discard)
        } else if let Some(f) = out_file.as_mut() {
            Some(f.writer())
        } else if opts.to_stdout || name == "-" {
            Some(&mut stdout_w)
        } else {
            None
        };
        match process(&opts, name, shared) {
            Ok(()) => {}
            Err(Fail::BrokenPipe) => return ExitCode::from(EXIT_PIPE),
            Err(Fail::Msg(m)) => {
                complain(&m);
                failed = true;
            }
        }
    }

    if let Err(e) = stdout_w.flush() {
        if e.kind() == io::ErrorKind::BrokenPipe {
            return ExitCode::from(EXIT_PIPE);
        }
        complain(&format!("stdout: {e}"));
        failed = true;
    }
    if let Some(staged) = out_file {
        let out = staged.out.clone();
        if failed {
            // Dropping the staged file removes it; nothing is published.
            drop(staged);
            complain(&format!("{}: not written because of the errors above", out.display()));
        } else if let Err(e) = staged.publish(opts.force, None) {
            complain(&publish_error(&out, &e));
            failed = true;
        }
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Opts {
        match parse(args.iter().map(OsString::from).collect(), Mode::Compress, false).unwrap() {
            Parsed::Run(o) => o,
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn parsing() {
        let o = p(&[]);
        assert_eq!(o.files, vec![OsString::from("-")]);
        assert_eq!(o.mode, Mode::Compress);
        let o = p(&["-dc1v", "a", "--", "-b"]);
        assert_eq!(o.mode, Mode::Decompress);
        assert!(o.to_stdout && o.verbose);
        assert_eq!(o.enc.order, Order::One);
        assert_eq!(o.files, vec![OsString::from("a"), OsString::from("-b")]);
        let o = p(&["-oout", "x"]);
        assert_eq!(o.output, Some(PathBuf::from("out")));
        let o = p(&["--output=out", "--order=0", "-"]);
        assert_eq!(o.output, Some(PathBuf::from("out")));
        assert_eq!(o.enc.order, Order::Zero);
        let o = p(&["-T3", "--block=1M"]);
        assert_eq!((o.enc.threads, o.enc.block_log2), (3, 20));
        let o = p(&["-T", "2", "--block=64K", "--memory=512M"]);
        assert_eq!((o.enc.threads, o.enc.block_log2, o.enc.memory), (2, 16, 512 << 20));
        assert!(parse(vec!["--memory=lots".into()], Mode::Compress, false).is_err());
        let auto = p(&[]).enc.memory;
        assert!(auto >= 1 << 20, "auto budget {auto}");
        assert_eq!(p(&["--memory=AUTO"]).enc.memory, auto);
        assert_eq!(p(&["--memory=auto", "--memory=1G"]).enc.memory, 1 << 30);
        assert_eq!(p(&["--memory=1G", "--memory", "auto"]).enc.memory, auto);
        // --dedup and --no-dedup: the last one given wins, in either order.
        assert_eq!(p(&["--dedup"]).enc.dedup_log2, format::DEFAULT_DEDUP_LOG2);
        assert_eq!(p(&["--dedup", "--no-dedup"]).enc.dedup_log2, 0);
        assert_eq!(p(&["--no-dedup", "--dedup"]).enc.dedup_log2, format::DEFAULT_DEDUP_LOG2);
        assert_eq!(p(&["--dedup=1G", "--no-dedup"]).enc.dedup_log2, 0);
        assert_eq!(p(&["--no-dedup", "--dedup=64M"]).enc.dedup_log2, 26);
        // Block and window sizes round up to a power of two; suffixes
        // work in either case.
        assert_eq!(p(&["--block=64K", "--dedup=3M"]).enc.dedup_log2, 22);
        assert_eq!(p(&["--block=64K", "--dedup=33m"]).enc.dedup_log2, 26);
        assert_eq!(p(&["--block=3M"]).enc.block_log2, 22);
        assert_eq!(p(&["--block=100k"]).enc.block_log2, 17);
        assert_eq!(p(&["--block=1g"]).enc.block_log2, 30);
        assert!(parse(vec!["--block=0".into()], Mode::Compress, false).is_err());
        for v in ["16M", "16m", "16MB", "16mb", "16MiB", "16mib"] {
            assert_eq!(parse_size(v), Some(16 << 20), "{v}");
        }
        assert_eq!(parse_size("4096"), Some(4096));
        assert_eq!(parse_size("4096B"), Some(4096));
        for v in ["", "B", "16iB", "16i", "16X", "M", "-1", "16 M", "16MiBB"] {
            assert_eq!(parse_size(v), None, "{v:?}");
        }
        assert!(parse(vec!["--block=1025M".into()], Mode::Compress, false).is_err());
        assert!(parse(vec!["--dedup=0".into()], Mode::Compress, false).is_err());
        assert!(parse(vec!["--dedup=128G".into()], Mode::Compress, false).is_err());
        // A window asked to be larger than the block stays larger after
        // rounding; one not asked to be larger is refused, sizes named.
        assert_eq!(p(&["--block=3M", "--dedup=4M"]).enc.dedup_log2, 23);
        assert_eq!(p(&["--dedup=5M", "--block=3M"]).enc.dedup_log2, 23);
        let e = match parse(vec!["--block=1G".into(), "--dedup".into()], Mode::Compress, false) {
            Err(e) => e,
            Ok(_) => panic!("accepted"),
        };
        assert!(e.contains("(256M)") && e.contains("(1G)"), "{e}");
        // The window must exceed the block, whatever the order of options.
        assert!(parse(vec!["--dedup=16M".into(), "--block=16M".into()], Mode::Compress, false).is_err());
        assert!(parse(vec!["--block=16M".into(), "--dedup=16M".into()], Mode::Compress, false).is_err());
        assert!(parse(vec!["--no-dedup=1".into()], Mode::Compress, false).is_err());
        assert!(parse(vec!["--block=2G".into()], Mode::Compress, false).is_err());
        assert!(parse(vec!["-x".into()], Mode::Compress, false).is_err());
        // Long options take "--opt=V" or "--opt V"; --dedup only "=".
        let o = p(&["--threads", "5", "--block", "1M", "--memory", "1G", "--order", "2", "--output", "o"]);
        assert_eq!((o.enc.threads, o.enc.block_log2, o.enc.memory), (5, 20, 1 << 30));
        assert_eq!((o.enc.order, o.output), (Order::Two, Some(PathBuf::from("o"))));
        assert_eq!(p(&["--dedup", "x"]).files, vec![OsString::from("x")]);
        assert!(parse(vec!["--threads".into()], Mode::Compress, false).is_err());
        assert!(parse(vec!["-c".into(), "-o".into(), "f".into()], Mode::Compress, false).is_err());
    }

    #[test]
    fn cgroup_limit_follows_the_mount() {
        let base = std::env::temp_dir().join(format!("acz-cgroup-{}", std::process::id()));
        let mnt = base.join("cg root");
        fs::create_dir_all(mnt.join("a/b/c")).unwrap();
        fs::write(mnt.join("memory.max"), "max\n").unwrap();
        fs::write(mnt.join("a/memory.max"), "3000\n").unwrap();
        fs::write(mnt.join("a/b/memory.max"), "max\n").unwrap();
        fs::write(mnt.join("a/b/c/memory.max"), "5000\n").unwrap();
        // This file is outside either mounted hierarchy and must not bind.
        fs::write(base.join("memory.max"), "1\n").unwrap();
        let m = mnt.to_str().unwrap().replace(' ', "\\040");
        let info = |root: &str| format!("30 1 0:26 {root} {m} rw,nosuid shared:4 - cgroup2 cgroup2 rw\n");
        // The whole hierarchy mounted: an ancestor's limit binds.
        assert_eq!(cgroup_limit_in("0::/a/b/c\n", &info("/")), Some(3000));
        assert_eq!(cgroup_limit_in("0::/a/b\n", &info("/")), Some(3000));
        // Only the subtree /work is mounted: /work/a/b/c is mnt/a/b/c.
        assert_eq!(cgroup_limit_in("0::/work/a/b/c\n", &info("/work")), Some(3000));
        assert_eq!(cgroup_limit_in("0::/work\n", &info("/work")), None);
        // A cgroup namespace: the process sees itself at the root.
        assert_eq!(cgroup_limit_in("0::/\n", &info("/")), None);
        // Not under the mount's root, no cgroup2 mount, or cgroup v1 only.
        assert_eq!(cgroup_limit_in("0::/other\n", &info("/work")), None);
        assert_eq!(cgroup_limit_in("0::/a\n", "30 1 0:26 / /x rw - tmpfs t rw\n"), None);
        assert_eq!(cgroup_limit_in("1:memory:/a\n", &info("/")), None);
        assert_eq!(cgroup_limit_in("0::/../outside\n", &info("/")), None);
        // Whitespace belongs to the cgroup name; do not trim membership.
        fs::create_dir_all(mnt.join("a/trailing ")).unwrap();
        fs::write(mnt.join("a/trailing /memory.max"), "500\n").unwrap();
        assert_eq!(cgroup_limit_in("0::/work/a/trailing \n", &info("/work")), Some(500));

        // A subtree bind mount must not hide a stricter ancestor available
        // through another mount, whatever order mountinfo lists them in.
        let full = base.join("whole");
        fs::create_dir_all(full.join("tenant/work/a/b/c")).unwrap();
        fs::write(full.join("tenant/memory.max"), "1000\n").unwrap();
        fs::write(full.join("tenant/work/a/memory.max"), "3000\n").unwrap();
        fs::write(full.join("tenant/work/a/b/c/memory.max"), "5000\n").unwrap();
        let whole = format!("31 1 0:26 / {} rw - cgroup2 cgroup2 rw\n", full.display());
        let subtree = info("/tenant/work");
        for mounts in [format!("{whole}{subtree}"), format!("{subtree}{whole}")] {
            assert_eq!(cgroup_limit_in("0::/tenant/work/a/b/c\n", &mounts), Some(1000));
        }
        let absent = format!("32 1 0:26 /tenant/work {} rw - cgroup2 cgroup2 rw\n", base.join("missing").display());
        for mounts in [format!("{whole}{absent}"), format!("{absent}{whole}")] {
            assert_eq!(cgroup_limit_in("0::/tenant/work/a/b/c\n", &mounts), Some(1000));
        }
        fs::remove_dir_all(&base).unwrap();
    }
}
