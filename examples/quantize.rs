//! Quantizes potion's f32 embedding table to int8, one-off.
//!
//! `cargo run --release --example quantize -- models/potion-base-8M`
//!
//! Static embeddings are a token -> vector lookup table, so quantization here is
//! a plain per-row affair: each token's row gets its own scale, chosen so the
//! largest magnitude in that row lands on 127. `model2vec-rs` multiplies the
//! decoded int8 by that scale at lookup time (its `weights` tensor), which is
//! exactly the reconstruction this produces.
//!
//! Per-row rather than one global scale matters: token norms in these tables
//! vary by more than an order of magnitude, and a single scale would flatten
//! rare tokens -- precisely the ones carrying the most retrieval signal.
//!
//! The run prints the reconstruction error it measured. Trust the number, not
//! the intent.

use safetensors::tensor::{Dtype, TensorView};
use std::collections::HashMap;

fn main() -> anyhow::Result<()> {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "models/potion-base-8M".into());
    let dir = std::path::Path::new(&dir);

    let raw = std::fs::read(dir.join("model.safetensors"))?;
    let st = safetensors::SafeTensors::deserialize(&raw)?;
    let t = st
        .tensor("embeddings")
        .or_else(|_| st.tensor("0"))
        .or_else(|_| st.tensor("embedding.weight"))?;

    anyhow::ensure!(
        t.dtype() == Dtype::F32,
        "expected an f32 table, found {:?}",
        t.dtype()
    );
    let [rows, cols]: [usize; 2] = t.shape().try_into().unwrap();
    let floats: Vec<f32> = t
        .data()
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    println!("input : {rows} x {cols} f32  ({} bytes)", raw.len());

    let mut q = vec![0i8; rows * cols];
    let mut scales = vec![0f32; rows];
    for r in 0..rows {
        let row = &floats[r * cols..(r + 1) * cols];
        let peak = row.iter().fold(0f32, |m, v| m.max(v.abs()));
        // An all-zero row has no scale worth recording; leave it at zero so
        // reconstruction stays exactly zero rather than becoming NaN.
        let scale = if peak == 0.0 { 0.0 } else { peak / 127.0 };
        scales[r] = scale;
        if scale > 0.0 {
            for (c, v) in row.iter().enumerate() {
                q[r * cols + c] = (v / scale).round().clamp(-127.0, 127.0) as i8;
            }
        }
    }

    // Cosine similarity between each original row and its reconstruction. Cosine
    // is the right measure because retrieval only ever compares directions.
    let (mut worst, mut mean) = (1.0f32, 0.0f64);
    let mut counted = 0usize;
    for r in 0..rows {
        let orig = &floats[r * cols..(r + 1) * cols];
        if scales[r] == 0.0 {
            continue;
        }
        let recon: Vec<f32> = (0..cols)
            .map(|c| f32::from(q[r * cols + c]) * scales[r])
            .collect();
        let dot: f32 = orig.iter().zip(&recon).map(|(a, b)| a * b).sum();
        let na = orig.iter().map(|v| v * v).sum::<f32>().sqrt();
        let nb = recon.iter().map(|v| v * v).sum::<f32>().sqrt();
        let cos = dot / (na * nb);
        worst = worst.min(cos);
        mean += f64::from(cos);
        counted += 1;
    }
    println!(
        "recon : mean cosine {:.6}, worst row {:.6} over {counted} non-zero rows",
        mean / counted as f64,
        worst
    );

    let q_bytes: Vec<u8> = q.iter().map(|&v| v as u8).collect();
    let s_bytes: Vec<u8> = scales.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut tensors: HashMap<String, TensorView> = HashMap::new();
    tensors.insert(
        "embeddings".into(),
        TensorView::new(Dtype::I8, vec![rows, cols], &q_bytes)?,
    );
    // `model2vec-rs` reads this as the per-token scale and multiplies it in at
    // lookup time.
    tensors.insert(
        "weights".into(),
        TensorView::new(Dtype::F32, vec![rows], &s_bytes)?,
    );

    let out = dir.join("model.int8.safetensors");
    safetensors::serialize_to_file(&tensors, None, &out)?;
    let size = std::fs::metadata(&out)?.len();
    println!(
        "output: {} ({size} bytes, {:.1}% of input)",
        out.display(),
        size as f64 / raw.len() as f64 * 100.0
    );
    Ok(())
}
