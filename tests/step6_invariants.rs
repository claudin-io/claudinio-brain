//! Passo 6, part 2: what must hold for *any* graph.
//!
//! The example-based tests pin down the shapes we thought of -- a chain, a cycle,
//! a hub. A real brain grows shapes nobody drew: mutual references, dense
//! clusters, an entity that points at itself. These properties are what say the
//! walk is safe on those too.

use brain::brain::Brain;
use brain::clock::StepClock;
use brain::ids::SeededIdGen;
use brain::recall::When;
use jiff::Timestamp;
use proptest::prelude::*;

const EPOCH: &str = "2026-01-01T00:00:00Z";

/// Entities are drawn from a small pool on purpose: a pool this size makes
/// cycles, self-loops and multi-edges likely rather than rare.
const POOL: usize = 6;

fn edges_strategy() -> impl Strategy<Value = Vec<(usize, usize)>> {
    prop::collection::vec((0..POOL, 0..POOL), 0..16)
}

fn build(edges: &[(usize, usize)]) -> Brain {
    let dir = tempfile::TempDir::new().unwrap();
    let b = Brain::init(
        &dir.path().join("t.db"),
        "prop",
        Box::new(StepClock::new(EPOCH.parse::<Timestamp>().unwrap(), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap();
    for (i, (from, to)) in edges.iter().enumerate() {
        // A distinct relation per edge keeps single-valued supersession from
        // quietly closing the edges this test just created.
        b.link(
            &format!("n{from}"),
            &format!("rel_{i}"),
            &format!("n{to}"),
            Some(EPOCH.parse().unwrap()),
        )
        .unwrap();
    }
    b
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Termination is the property. A recursive CTE over a cyclic graph is the
    /// one place in this codebase where a wrong query does not return a wrong
    /// answer -- it never returns at all.
    #[test]
    fn a_walk_terminates_and_respects_its_depth(edges in edges_strategy(), depth in 0u32..3) {
        let b = build(&edges);
        for i in 0..POOL {
            let Some(view) = b.entity(&format!("n{i}"), When::Now, depth).unwrap() else {
                continue;
            };

            for n in &view.neighbours {
                prop_assert!(n.hops >= 1 && n.hops <= depth, "hop {} outside 1..={depth}", n.hops);
                prop_assert_ne!(&n.key, &format!("n{i}"), "an anchor is not its own neighbour");
            }

            let keys: Vec<&str> = view.neighbours.iter().map(|n| n.key.as_str()).collect();
            let mut unique = keys.clone();
            unique.sort_unstable();
            unique.dedup();
            prop_assert_eq!(keys.len(), unique.len(), "an entity was reported twice");
        }
    }

    /// Reachability only grows with depth. A neighbour found at depth one that
    /// vanishes at depth two would mean the walk is losing paths rather than
    /// adding them -- the kind of bug that shows up as an answer that comes and
    /// goes depending on a constant.
    #[test]
    fn a_deeper_walk_never_loses_a_neighbour(edges in edges_strategy()) {
        let b = build(&edges);
        for i in 0..POOL {
            let name = format!("n{i}");
            let Some(shallow) = b.entity(&name, When::Now, 1).unwrap() else { continue };
            let deep = b.entity(&name, When::Now, 2).unwrap().unwrap();

            let reached: Vec<&str> = deep.neighbours.iter().map(|n| n.key.as_str()).collect();
            for n in &shallow.neighbours {
                prop_assert!(reached.contains(&n.key.as_str()), "lost {:?} at depth 2", n.key);
            }
        }
    }
}
