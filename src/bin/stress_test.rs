//! Stress test benchmark comparing Rust decoder performance
//!
//! Usage:
//!   cargo run --release --bin stress_test --features unsafe-simd
//!
//! This benchmark decodes multiple HEIC images multiple times to get
//! accurate performance measurements. Results can be compared against
//! libde265/libheif for validation.

use heic_decoder::HeicDecoder;
use std::time::{Duration, Instant};

struct BenchmarkResult {
    file_name: String,
    file_size_mb: f64,
    width: u32,
    height: u32,
    iterations: usize,
    total_time: Duration,
    avg_time: Duration,
    min_time: Duration,
    max_time: Duration,
    megapixels_per_sec: f64,
}

impl BenchmarkResult {
    fn print(&self) {
        println!("\n{}", "=".repeat(70));
        println!("File: {}", self.file_name);
        println!("Size: {:.2} MB", self.file_size_mb);
        println!("Dimensions: {}x{} ({:.2} MP)",
            self.width, self.height,
            (self.width * self.height) as f64 / 1_000_000.0);
        println!("{}", "-".repeat(70));
        println!("Iterations: {}", self.iterations);
        println!("Total time: {:.3}s", self.total_time.as_secs_f64());
        println!("Average:    {:.3}s per decode", self.avg_time.as_secs_f64());
        println!("Min:        {:.3}s", self.min_time.as_secs_f64());
        println!("Max:        {:.3}s", self.max_time.as_secs_f64());
        println!("Throughput: {:.2} MP/s", self.megapixels_per_sec);
        println!("{}", "=".repeat(70));
    }
}

fn benchmark_file(path: &str, iterations: usize) -> Result<BenchmarkResult, Box<dyn std::error::Error>> {
    println!("\n🔄 Loading {}...", path);
    let data = std::fs::read(path)?;
    let file_size_mb = data.len() as f64 / (1024.0 * 1024.0);

    let decoder = HeicDecoder::new();

    // Get image info
    let info = decoder.get_info(&data)?;
    println!("   Dimensions: {}x{}", info.width, info.height);

    let megapixels = (info.width * info.height) as f64 / 1_000_000.0;

    println!("   Running {} iterations...", iterations);

    let mut times = Vec::with_capacity(iterations);
    let mut total_time = Duration::ZERO;

    for i in 0..iterations {
        let start = Instant::now();
        let _result = decoder.decode(&data)?;
        let elapsed = start.elapsed();

        times.push(elapsed);
        total_time += elapsed;

        if (i + 1) % 10 == 0 || i == 0 {
            print!("   [{:3}/{}] {:.3}s", i + 1, iterations, elapsed.as_secs_f64());
            if i > 0 {
                let avg_so_far = total_time / (i + 1) as u32;
                print!(" (avg: {:.3}s)", avg_so_far.as_secs_f64());
            }
            println!();
        }
    }

    let avg_time = total_time / iterations as u32;
    let min_time = *times.iter().min().unwrap();
    let max_time = *times.iter().max().unwrap();
    let megapixels_per_sec = megapixels / avg_time.as_secs_f64();

    Ok(BenchmarkResult {
        file_name: path.to_string(),
        file_size_mb,
        width: info.width,
        height: info.height,
        iterations,
        total_time,
        avg_time,
        min_time,
        max_time,
        megapixels_per_sec,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n╔════════════════════════════════════════════════════════════════════╗");
    println!("║         HEIC Decoder Stress Test Benchmark                        ║");
    println!("╚════════════════════════════════════════════════════════════════════╝");

    // Check SIMD status
    #[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
    {
        if std::is_x86_feature_detected!("avx2") {
            println!("\n✅ SIMD: AVX2 enabled and detected");
        } else {
            println!("\n⚠️  SIMD: Feature enabled but AVX2 not available (using scalar fallback)");
        }
    }
    #[cfg(not(feature = "unsafe-simd"))]
    {
        println!("\n⚠️  SIMD: Not enabled (compile with --features unsafe-simd for best performance)");
    }

    let test_files = vec![
        ("libheif/examples/example.heic", 10),
        ("20240601_170601.heic", 10),
    ];

    let mut results = Vec::new();

    for (path, iterations) in test_files {
        match benchmark_file(path, iterations) {
            Ok(result) => {
                result.print();
                results.push(result);
            }
            Err(e) => {
                eprintln!("\n❌ Failed to benchmark {}: {}", path, e);
                eprintln!("   Skipping this file...");
            }
        }
    }

    // Summary
    if !results.is_empty() {
        println!("\n╔════════════════════════════════════════════════════════════════════╗");
        println!("║                         SUMMARY                                    ║");
        println!("╚════════════════════════════════════════════════════════════════════╝\n");

        let total_iterations: usize = results.iter().map(|r| r.iterations).sum();
        let total_time: Duration = results.iter().map(|r| r.total_time).sum();
        let avg_throughput: f64 = results.iter().map(|r| r.megapixels_per_sec).sum::<f64>() / results.len() as f64;

        println!("Total iterations:  {}", total_iterations);
        println!("Total decode time: {:.3}s", total_time.as_secs_f64());
        println!("Avg throughput:    {:.2} MP/s", avg_throughput);

        println!("\n📊 Per-file average decode times:");
        for result in &results {
            let filename = std::path::Path::new(&result.file_name)
                .file_name()
                .unwrap()
                .to_string_lossy();
            println!("   {:<30} {:.3}s  ({:.2} MP/s)",
                filename,
                result.avg_time.as_secs_f64(),
                result.megapixels_per_sec);
        }

        println!("\n💡 To compare with libde265:");
        println!("   1. Build libde265: cd libde265 && cmake . && make");
        println!("   2. Decode with libde265: time ./dec265 -q input.heic");
        println!("   3. Compare decode times");
    }

    Ok(())
}
