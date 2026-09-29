//! The source of the generator's random choices.
//!
//! Normally the choices come from a seeded ChaCha generator. For
//! coverage-guided fuzzing they come from the fuzzer's input bytes instead,
//! so that the fuzzer mutates the *choices* of the generator rather than the
//! query text ("semantic fuzzing", as in Zest/JQF). Every byte string maps
//! to a valid query, because validity is ensured by the generator.

use std::convert::Infallible;

use rand::TryRng;
use rand_chacha::ChaCha8Rng;

/// Where the random choices come from.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // one per generated query
pub(crate) enum ChoiceSource {
    /// A seeded pseudo-random generator.
    Seeded(ChaCha8Rng),
    /// The bytes of a fuzzer input, consumed front to back. Once they are
    /// exhausted, all further bytes are `0xFF`, which makes every "generate
    /// more?" decision say no, so that short inputs give small queries.
    Bytes {
        /// The input.
        data: Vec<u8>,
        /// The position of the next unread byte.
        position: usize,
    },
}

impl ChoiceSource {
    /// Choices read from `data`.
    pub fn from_bytes(data: &[u8]) -> Self {
        Self::Bytes {
            data: data.to_vec(),
            position: 0,
        }
    }

    fn fill(&mut self, dst: &mut [u8]) {
        match self {
            Self::Seeded(rng) => rand::Rng::fill_bytes(rng, dst),
            Self::Bytes { data, position } => {
                for b in dst {
                    *b = data.get(*position).copied().unwrap_or(0xFF);
                    *position = position.saturating_add(1);
                }
            }
        }
    }
}

impl TryRng for ChoiceSource {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        if let Self::Seeded(rng) = self {
            return Ok(rand::Rng::next_u32(rng));
        }
        let mut bytes = [0; 4];
        self.fill(&mut bytes);
        Ok(u32::from_le_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        if let Self::Seeded(rng) = self {
            return Ok(rand::Rng::next_u64(rng));
        }
        let mut bytes = [0; 8];
        self.fill(&mut bytes);
        Ok(u64::from_le_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        self.fill(dst);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rand::{Rng, RngExt};

    use super::*;

    #[test]
    fn bytes_are_read_in_order_then_saturate() {
        let mut c = ChoiceSource::from_bytes(&[1, 0, 0, 0, 2]);
        assert_eq!(c.next_u32(), 1);
        assert_eq!(c.next_u32(), 0xFFFF_FF02);
        assert_eq!(c.next_u32(), u32::MAX);
        // Exhausted input stops optional growth.
        assert!(!c.random_bool(0.9));
    }
}
