//! Turning text into vectors.
//!
//! The model is a **static embedding** table -- Model2Vec's `potion-base-8M`,
//! distilled into a plain token to vector lookup. There is no transformer
//! inference at query time: tokenize, gather rows, average, normalize.
//!
//! That choice is the answer to "efficient and lightweight":
//!
//! - about 7.7 MB of int8 weights compiled into the binary, no download, no
//!   `.onnx` runtime, no C++ toolchain
//! - roughly 8000 embeddings/sec on one core, so re-embedding on every write
//!   costs about 0.12 ms and never needs to be deferred
//! - deterministic by construction -- no sampling, no dropout -- which is what
//!   lets the eval baselines and snapshot tests exist at all
//!
//! The cost is quality: static embeddings score around 82% of MiniLM on MTEB
//! retrieval, because a lookup table cannot disambiguate a word from its
//! context. That is exactly why this is one fused channel among several rather
//! than the whole retrieval story, and why `evals/` prices it rather than
//! assuming it earns its place.

use crate::brain::BrainError;

pub trait Embedder: Send + Sync {
    fn dim(&self) -> usize;
    /// Stored alongside every vector, so a model change is detectable and a
    /// re-embed can be a background pass rather than a migration.
    fn model_id(&self) -> &str;
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, BrainError>;

    fn embed_one(&self, text: &str) -> Result<Vec<f32>, BrainError> {
        Ok(self.embed(&[text])?.pop().unwrap_or_default())
    }
}

/// The weights, compiled in. `local-only` on `model2vec-rs` cfg-deletes every
/// network path, so this cannot reach out even if asked to.
const MODEL: &[u8] = include_bytes!("../../models/potion-base-8M/model.int8.safetensors");
const TOKENIZER: &[u8] = include_bytes!("../../models/potion-base-8M/tokenizer.json");
const CONFIG: &[u8] = include_bytes!("../../models/potion-base-8M/config.json");

pub const MODEL_ID: &str = "potion-base-8M-int8";
pub const DIM: usize = 256;

pub struct StaticEmbedder {
    model: model2vec_rs::model::StaticModel,
}

impl StaticEmbedder {
    pub fn bundled() -> Result<Self, BrainError> {
        let model = model2vec_rs::model::StaticModel::from_bytes(
            TOKENIZER,
            MODEL,
            CONFIG,
            // Normalize, so cosine similarity is a plain dot product and int8
            // storage can assume components live in [-1, 1].
            Some(true),
        )
        .map_err(|e| BrainError::Embedding(e.to_string()))?;
        Ok(Self { model })
    }
}

impl Embedder for StaticEmbedder {
    fn dim(&self) -> usize {
        DIM
    }

    fn model_id(&self) -> &str {
        MODEL_ID
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, BrainError> {
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_string()).collect();
        let mut out = self.model.encode(&owned);

        // A string with no known tokens yields an all-zero vector, whose cosine
        // against anything is NaN. Returning a zero vector is correct -- it
        // matches nothing -- but the distance maths must not produce NaN, so pad
        // to the expected width and leave it at zero.
        for v in &mut out {
            if v.len() != DIM {
                v.resize(DIM, 0.0);
            }
        }
        Ok(out)
    }
}

/// Packs a normalized f32 vector into the int8 layout `vec0` stores.
///
/// Safe because the embedder normalizes: every component is in [-1, 1], so a
/// single scale of 127 is exact for the whole table. The clamp guards the
/// boundary case where rounding would overflow to 128.
pub fn to_int8(v: &[f32]) -> Vec<i8> {
    v.iter()
        .map(|x| (x * 127.0).round().clamp(-127.0, 127.0) as i8)
        .collect()
}

pub fn to_bytes(v: &[f32]) -> Vec<u8> {
    bytemuck::cast_slice(&to_int8(v)).to_vec()
}

pub fn from_bytes(b: &[u8]) -> Vec<f32> {
    bytemuck::cast_slice::<u8, i8>(b)
        .iter()
        .map(|&x| f32::from(x) / 127.0)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int8_round_trip_keeps_direction() {
        let e = StaticEmbedder::bundled().unwrap();
        let v = e.embed_one("qual o preco do produto A").unwrap();
        let back = from_bytes(&to_bytes(&v));

        let dot: f32 = v.iter().zip(&back).map(|(a, b)| a * b).sum();
        let na: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = back.iter().map(|x| x * x).sum::<f32>().sqrt();
        let cos = dot / (na * nb);
        assert!(cos > 0.999, "int8 round trip lost direction: cos = {cos}");
    }

    #[test]
    fn an_all_zero_vector_survives_the_round_trip() {
        let z = vec![0.0f32; DIM];
        assert_eq!(from_bytes(&to_bytes(&z)), z);
    }
}
