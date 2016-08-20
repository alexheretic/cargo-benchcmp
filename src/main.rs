extern crate rustc_serialize;
extern crate docopt;
#[macro_use]
extern crate lazy_static;
extern crate regex;
#[macro_use]
extern crate prettytable;
#[cfg(test)]
#[macro_use]
extern crate quickcheck;

use std::io::{self, BufRead, Read};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process;

use docopt::Docopt;
use prettytable::Table;
use prettytable::format;

use benchmark::{Benchmarks, Benchmark};
use error::{Result, Error};

mod benchmark;
mod error;

macro_rules! eprintln {
    ($($tt:tt)*) => {{
        use std::io::{Write, stderr};
        writeln!(&mut stderr(), $($tt)*).unwrap();
    }}
}

const USAGE: &'static str = r#"
Compares Rust micro-benchmark results.

Usage:
    cargo benchcmp [options] <old> <new>
    cargo benchcmp [options] <old> <new> <file>
    cargo benchcmp -h | --help
    cargo benchcmp --version

The first version takes two files and compares the common benchmarks.

The second version takes two benchmark name prefixes and one benchmark output
file, and compares the common benchmarks (as determined by comparing the
benchmark names with their prefixes stripped). Benchmarks not matching either
prefix are ignored completely.

If benchmark output is sent on stdin, then the second version is used and the
third file parameter is not needed.

Options:
    -h, --help           Show this help message and exit.
    --version            Show the version.
    --threshold <n>      Show only comparisons with a percentage change greater
                         than this threshold.
    --variance           Show the variance of each benchmark.
    --improvements       Show only improvements.
    --regressions        Show only regressions.
    --color <when>       Show colored rows: never, always or auto [default: auto]
"#;

#[derive(Debug, RustcDecodable)]
struct Args {
    arg_old: String,
    arg_new: String,
    arg_file: Option<String>,
    flag_threshold: Option<u8>,
    flag_variance: bool,
    flag_improvements: bool,
    flag_regressions: bool,
    flag_color: When,
}

#[derive(Debug, RustcDecodable)]
enum When {
    Never, Always, Auto
}

fn main() {
    let args: Args = Docopt::new(USAGE)
        .and_then(|d| d.version(Some(version())).decode())
        .unwrap_or_else(|e| e.exit());
    if let Err(e) = args.run() {
        eprintln!("{}", e);
        process::exit(1);
    }
}

impl Args {
    fn run(&self) -> Result<()> {
        let (name_old, name_new) = Args::names(&self.arg_old, &self.arg_new);
        let benches = try!(self.parse_benchmarks()).paired();
        let mut output = Table::new();
        output.set_format(*format::consts::FORMAT_CLEAN);
        output.add_row(row![
            b->"name",
            b->format!("{} ns/iter", name_old),
            b->format!("{} ns/iter", name_new),
            br->"diff ns/iter",
            br->"diff %"
        ]);
        for c in benches.comparisons() {
            let abs_per = (c.diff_ratio * 100f64).abs().trunc() as u8;
            let regression = c.diff_ns < 0;
            if self.flag_threshold.map_or(false, |t| abs_per < t)
                || self.flag_regressions && regression
                || self.flag_improvements && !regression {
                continue;
            }
            output.add_row(c.to_row(self.flag_variance, regression));
        }

        match self.flag_color {
            When::Auto => output.printstd(),
            When::Never => try!(output.print(&mut io::stdout())),
            When::Always => output.print_tty(true),
        }

        // If there were any unpaired benchmarks, show them now.
        if !benches.missing_old().is_empty() {
            let missed = benches
                .missing_old().iter().map(|b| b.name.to_string())
                .collect::<Vec<String>>().join(", ");
            eprintln!("WARNING: benchmarks in old but not in new: {}", missed);
        }
        if !benches.missing_new().is_empty() {
            let missed = benches
                .missing_new().iter().map(|b| b.name.to_string())
                .collect::<Vec<String>>().join(", ");
            eprintln!("WARNING: benchmarks in new but not in old: {}", missed);
        }
        Ok(())
    }

    /// Parse benchmarks from the command line invocation given.
    fn parse_benchmarks(&self) -> Result<Benchmarks> {
        if let Some(ref one_file) = self.arg_file {
            if one_file == "-" {
                let mut buf = String::new();
                let stdin = io::stdin();
                try!(stdin.lock().read_to_string(&mut buf));
                self.parse_buf_benchmarks(&buf)
            } else {
                self.parse_file_benchmarks(one_file)
            }
        } else {
            self.parse_old_new_benchmarks()
        }
    }

    /// Parses benchmarks from two files: one containing old benchmark output
    /// and another containing new benchmark output.
    fn parse_old_new_benchmarks(&self) -> Result<Benchmarks> {
        let bold = io::BufReader::new(try!(open_file(&self.arg_old)));
        let bnew = io::BufReader::new(try!(open_file(&self.arg_new)));

        let mut benches = Benchmarks::new();
        for line in bold.lines() {
            let line = try!(line);
            if let Ok(bench) = line.parse() {
                benches.add_old(bench);
            }
        }
        for line in bnew.lines() {
            let line = try!(line);
            if let Ok(bench) = line.parse() {
                benches.add_new(bench);
            }
        }
        Ok(benches)
    }

    /// Parses benchmarks from one file with two prefixes. The first prefix
    /// identifies benchmarks in the old set and the second prefix identifies
    /// benchmarks in the new set where all benchmarks are found in one file.
    fn parse_file_benchmarks<P>(
        &self,
        file: P,
    ) -> Result<Benchmarks>
    where P: AsRef<Path> {
        // Slurp up the entire file so that we can reuse this code with the
        // code for reading benchmarks on stdin.
        let mut buf = String::new();
        try!(try!(File::open(file)).read_to_string(&mut buf));
        self.parse_buf_benchmarks(&buf)
    }

    /// Same as parse_file_benchmarks, but straight from the buffer.
    fn parse_buf_benchmarks(&self, buf: &str) -> Result<Benchmarks> {
        let mut benches = Benchmarks::new();
        for line in buf.lines() {
            let mut bench: Benchmark = match line.parse() {
                Err(_) => continue,
                Ok(bench) => bench,
            };
            if bench.name.starts_with(&self.arg_old) {
                bench.name = bench.name[self.arg_old.len()..].to_string();
                benches.add_old(bench);
            } else if bench.name.starts_with(&self.arg_new) {
                bench.name = bench.name[self.arg_new.len()..].to_string();
                benches.add_new(bench);
            }
        }
        Ok(benches)
    }

    /// Returns the names that should be used in the column header.
    fn names(arg_old: &String, arg_new: &String) -> (String, String) {
        // If either of the names are empty, substitute them with defaults.
        let arg_old =
            if arg_old.is_empty() {
                "old".to_string()
            } else {
                arg_old.to_string()
            };
        let arg_new =
            if arg_new.is_empty() {
                "new".to_string()
            } else {
                arg_new.to_string()
            };
        // The names could be either in the prefixes or in the file paths.
        let (old, new) = (Path::new(&arg_old), Path::new(&arg_new));
        // No files paths? Don't do anything smart.
        if old.iter().count() <= 1 || new.iter().count() <= 1 {
            return (arg_old.clone(), arg_new.clone());
        }
        // If we have file paths, try to find the shortest string that
        // differentiates them.
        let (mut uold, mut unew) = (vec![], vec![]);
        for (o, n) in old.iter().rev().zip(new.iter().rev()) {
            uold.push(o.to_string_lossy().into_owned());
            unew.push(n.to_string_lossy().into_owned());
            if o != n {
                break;
            }
        }
        // If for some reason one of these is empty, just fall back to the
        // names given.
        if uold.is_empty() || unew.is_empty() {
            return (arg_old.clone(), arg_new.clone());
        }
        uold.reverse();
        unew.reverse();
        let pold: PathBuf = uold.into_iter().collect();
        let pnew: PathBuf = unew.into_iter().collect();
        (pold.display().to_string(), pnew.display().to_string())
    }
}

fn version() -> String {
    let (maj, min, pat) = (
        option_env!("CARGO_PKG_VERSION_MAJOR"),
        option_env!("CARGO_PKG_VERSION_MINOR"),
        option_env!("CARGO_PKG_VERSION_PATCH"),
    );
    match (maj, min, pat) {
        (Some(maj), Some(min), Some(pat)) =>
            format!("{}.{}.{}", maj, min, pat),
        _ => "".to_owned(),
    }
}

/// `open_file` is like `File::open`, except it gives a better error message
/// when it fails (i.e., it includes the file path).
fn open_file<P: AsRef<Path>>(path: P) -> Result<File> {
    File::open(&path).map_err(|err| {
        Error::OpenFile {
            path: path.as_ref().to_path_buf(),
            err: err,
        }
    })
}

#[cfg(test)]
mod tests {
    mod names {
        use super::super::Args;
        use std::path::{Path, PathBuf};
        use quickcheck::Arbitrary;
        use quickcheck::Gen;
        use std::ffi::OsStr;

        #[derive(Clone, Debug)]
        struct AlphaString(String);

        impl Arbitrary for AlphaString {
            fn arbitrary<G: Gen>(g: &mut G) -> Self {
                let size = g.size();
                let size = g.gen_range(1, size);
                AlphaString(g.gen_ascii_chars().take(size).collect())
            }
        }

        #[derive(Clone, Debug)]
        struct ArbitraryPathBuf(PathBuf);

        impl Arbitrary for ArbitraryPathBuf {
            fn arbitrary<G: Gen>(g: &mut G) -> Self {
                let components = g.size();
                let components = g.gen_range(1, components);
                let mut path_buf = PathBuf::new();
                for _ in 0 .. components {
                    let AlphaString(component) = AlphaString::arbitrary(g);
                    path_buf.push(component);
                }
                ArbitraryPathBuf(path_buf)
            }
        }

        #[derive(Clone, Debug)]
        struct ArbitraryPathBufPair(PathBuf, PathBuf);

        impl Arbitrary for ArbitraryPathBufPair {
            fn arbitrary<G: Gen>(g: &mut G) -> Self {
                let components = g.size();
                let components = g.gen_range(1, components);
                let mut path_buf1 = PathBuf::new();
                let mut path_buf2 = PathBuf::new();
                for component_no in 0 .. components {
                    let AlphaString(component) = AlphaString::arbitrary(g);
                    // further along in the path, the components are less likely to be different
                    if g.gen_weighted_bool(2 + (component_no / 2) as u32 ) {
                        path_buf1.push(component);
                        let AlphaString(component) = AlphaString::arbitrary(g);
                        path_buf2.push(component);
                    } else {
                        path_buf1.push(component.clone());
                        path_buf2.push(component);
                    }
                }
                ArbitraryPathBufPair(path_buf1, path_buf2)
            }
        }

        fn is_suffix_of(suffix: &String, string: &String) -> bool {
            let string = string;
            let suffix = suffix;
            string.len() >= suffix.len() || {
                let string_part = &string[string.len() - suffix.len()..];
                string_part == suffix
            }
        }

        quickcheck! {
            fn empty_gives_old(new_name: AlphaString) -> bool {
                let AlphaString(new_name) = new_name;
                let empty = String::from("");
                let result = Args::names(&empty, &new_name);

                ("old".to_string(), new_name) == result
            }

            fn empty_gives_new(old_name: AlphaString) -> bool {
                let AlphaString(old_name) = old_name;
                let empty = String::from("");
                let result = Args::names(&old_name, &empty);

                (old_name, "new".to_string()) == result
            }

            fn non_path_gives_originals(old_name: AlphaString, new_name: AlphaString) -> bool {
                let AlphaString(old_name) = old_name;
                let AlphaString(new_name) = new_name;
                let result = Args::names(&old_name, &new_name);

                (old_name, new_name) == result
            }

            fn same_path_gives_originals(path: ArbitraryPathBuf) -> bool {
                let ArbitraryPathBuf(path) = path;
                let path = path.to_string_lossy().into_owned();
                let result = Args::names(&path, &path);

                (path.clone(), path) == result
            }

            fn symmetric_operation(pair: ArbitraryPathBufPair) -> bool {
                let ArbitraryPathBufPair(old, new) = pair;
                let old = old.to_string_lossy().into_owned();
                let new = new.to_string_lossy().into_owned();
                let result = Args::names(&old, &new);

                (result.1, result.0) == Args::names(&new, &old)
            }

            fn difference_preserving(pair: ArbitraryPathBufPair) -> bool {
                let ArbitraryPathBufPair(old, new) = pair;
                let old = old.to_string_lossy().into_owned();
                let new = new.to_string_lossy().into_owned();
                let result = Args::names(&old, &new);

                (old == new) == (result.0 == result.1)
            }

            fn gives_suffixes(pair: ArbitraryPathBufPair) -> bool {
                let ArbitraryPathBufPair(old, new) = pair;
                let old = old.to_string_lossy().into_owned();
                let new = new.to_string_lossy().into_owned();
                let result = Args::names(&old, &new);

                is_suffix_of(&result.0, &old) && is_suffix_of(&result.1, &new)
            }

            fn shortest_difference(pair: ArbitraryPathBufPair) -> bool {
                let ArbitraryPathBufPair(old, new) = pair;
                let old = old.to_string_lossy().into_owned();
                let new = new.to_string_lossy().into_owned();
                let result = Args::names(&old, &new);

                let path_0 = Path::new(&result.0);
                let path_1 = Path::new(&result.1);
                let path_0: Vec<&OsStr> = path_0.iter().collect();
                let path_1: Vec<&OsStr> = path_1.iter().collect();
                let mut zipped = path_0.iter().rev().zip(path_1.iter().rev()).rev();
                let shortest_difference = zipped.next().map(|(o, n)| o != n).unwrap_or(false);
                let shortest_difference = shortest_difference && zipped.all(|(o, n)| o == n);

                old == new
                    || path_0.iter().count() <= 1
                    || path_1.iter().count() <= 1
                    || shortest_difference
            }
        }
    }
}
