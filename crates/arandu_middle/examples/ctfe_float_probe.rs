//! Reproducible dependency-cost probe; not a compiler throughput benchmark.
use std::{hint::black_box, time::Instant};

use arandu_middle::{
    DataLayout,
    ctfe::{ConstFloat, FloatArithmetic, FloatError, FloatType},
    types::Primitive,
};

fn main() -> Result<(), FloatError> {
    let ty = FloatType::new(Primitive::F64, DataLayout::ptr_width(8))?;
    let samples = [
        "0.1",
        "0.2",
        "3.141592653589793",
        "5e-324",
        "1.7976931348623157e308",
    ];
    let started = Instant::now();
    for index in 0..100_000 {
        black_box(
            black_box(samples[index % samples.len()])
                .parse::<f64>()
                .map_err(|_| FloatError::InvalidLiteral)?,
        );
    }
    println!("host parse, 100000: {:?}", started.elapsed());
    let started = Instant::now();
    for index in 0..100_000 {
        black_box(ConstFloat::parse(
            ty,
            black_box(samples[index % samples.len()]),
        )?);
    }
    println!("software parse, 100000: {:?}", started.elapsed());
    let started = Instant::now();
    let mut host = 0.0_f64;
    for _ in 0..100_000 {
        host = black_box(host) + black_box(0.1);
    }
    black_box(host);
    println!("host add, 100000: {:?}", started.elapsed());
    let mut software = ConstFloat::new(ty, 0)?;
    let tenth = ConstFloat::parse(ty, "0.1")?;
    let started = Instant::now();
    for _ in 0..100_000 {
        software = black_box(software).arithmetic(FloatArithmetic::Add, black_box(tenth))?;
    }
    black_box(software);
    println!("software add, 100000: {:?}", started.elapsed());
    Ok(())
}
