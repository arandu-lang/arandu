//! Replay explicit source-based rejection oracles without executing the program.

use std::io::Read;

pub fn run(mut args: impl Iterator<Item = String>) -> i32 {
    let Some(path) = args.next() else {
        eprintln!("check-negative-source: expected <source.aru> [mutation-name]");
        return 2;
    };
    let family = args.next();
    if args.next().is_some() {
        eprintln!("check-negative-source: unexpected argument");
        return 2;
    }
    let limit = 16 * 1024_u64;
    let source = (|| {
        let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let mut source = String::new();
        file.take(limit + 1)
            .read_to_string(&mut source)
            .map_err(|e| e.to_string())?;
        if u64::try_from(source.len()).unwrap_or(u64::MAX) > limit {
            return Err(format!("source exceeds {limit} bytes"));
        }
        Ok(source)
    })();
    let source = match source {
        Ok(source) => source,
        Err(error) => {
            eprintln!("check-negative-source: {error}");
            return 2;
        }
    };
    match arandu_fuzz_support::verify_negative_source(&source, family.as_deref()) {
        Ok(count) => {
            println!("check-negative-source: ok ({count} mutations)");
            0
        }
        Err(error @ arandu_fuzz_support::NegativeSourceError::Counterexample(_)) => {
            eprintln!("check-negative-source: {error}");
            1
        }
        Err(error) => {
            eprintln!("check-negative-source: {error}");
            2
        }
    }
}
