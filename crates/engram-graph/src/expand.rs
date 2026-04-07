//! Top-N graph expansion. v0 stub.

use uuid::Uuid;

/// Returns chunks reachable from a seed set within `max_hops`. v0 returns
/// the seed set unchanged.
pub fn expand_seeds(seeds: &[Uuid], _max_hops: u8) -> Vec<Uuid> {
    seeds.to_vec()
}
