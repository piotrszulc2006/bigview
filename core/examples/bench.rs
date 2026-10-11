//! Times indexing, a window read near the end, and a full-file search.
//! Usage: cargo run --release --example bench -- <file> [needle]

use std::time::Instant;

use bigview_core::BigFile;

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: bench <file> [needle]");
    let needle = args.next();

    let t = Instant::now();
    let file = BigFile::open(&path)?;
    let index = t.elapsed();
    println!(
        "{} bytes, {} lines, indexed in {:.0} ms ({:.2} GB/s)",
        file.len_bytes(),
        file.line_count(),
        index.as_secs_f64() * 1e3,
        file.len_bytes() as f64 / 1e9 / index.as_secs_f64()
    );

    let start = file.line_count().saturating_sub(40);
    let t = Instant::now();
    let window = file.lines(start, 40, 4096);
    println!("last {} lines read in {:.1} µs", window.len(), t.elapsed().as_secs_f64() * 1e6);

    if let Some(needle) = needle {
        let t = Instant::now();
        let hit = file.find_next(needle.as_bytes(), 0);
        println!("search for {needle:?}: {hit:?} in {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    }
    Ok(())
}
