//! Tests for `crates/deepseek-custom/src/evolution/mod.rs`.
//!
//! Pure unit tests against hand-built fitness numbers. No `StubBackend`
//! needed here: every function this module covers takes plain values in
//! and returns plain values out.

use deepseek_custom::evolution::*;

#[test]
fn candidate_no_features_is_not_archived() {
    let mut archive = MapElitesArchive::new(0.5);
    let c = Candidate { text: "a".into(), fitness: 1.0, features: vec![] };
    assert!(!archive.insert(c));
    assert!(archive.is_empty());
}

#[test]
fn first_candidate_wins_empty_cell() {
    let mut archive = MapElitesArchive::new(0.5);
    let c = Candidate { text: "a".into(), fitness: 1.0, features: vec![0.2] };
    assert!(archive.insert(c));
    assert_eq!(archive.len(), 1);
}

#[test]
fn higher_fitness_replaces_lower_in_same_cell() {
    let mut archive = MapElitesArchive::new(0.5);
    let lo = Candidate { text: "lo".into(), fitness: 1.0, features: vec![0.2] };
    let hi = Candidate { text: "hi".into(), fitness: 3.0, features: vec![0.3] };
    // 0.2/0.5 -> bucket 0, 0.3/0.5 -> bucket 0: same cell.
    assert!(archive.insert(lo));
    assert!(archive.insert(hi));
    assert_eq!(archive.len(), 1);
    assert_eq!(archive.best().unwrap().text, "hi");
}

#[test]
fn lower_fitness_loses_to_existing() {
    let mut archive = MapElitesArchive::new(0.5);
    let hi = Candidate { text: "hi".into(), fitness: 3.0, features: vec![0.2] };
    let lo = Candidate { text: "lo".into(), fitness: 1.0, features: vec![0.3] };
    assert!(archive.insert(hi));
    assert!(!archive.insert(lo));
    assert_eq!(archive.best().unwrap().text, "hi");
}

#[test]
fn different_cells_keep_both() {
    let mut archive = MapElitesArchive::new(1.0);
    let a = Candidate { text: "a".into(), fitness: 5.0, features: vec![0.5] }; // bucket 0
    let b = Candidate { text: "b".into(), fitness: 2.0, features: vec![1.5] }; // bucket 1
    assert!(archive.insert(a));
    assert!(archive.insert(b));
    assert_eq!(archive.len(), 2);
}

#[test]
fn two_dimensional_grid() {
    let mut archive = MapElitesArchive::new(1.0);
    let a = Candidate { text: "a".into(), fitness: 1.0, features: vec![0.5, 0.5] };
    let b = Candidate { text: "b".into(), fitness: 2.0, features: vec![0.5, 1.5] };
    let c = Candidate { text: "c".into(), fitness: 3.0, features: vec![1.5, 0.5] };
    assert!(archive.insert(a));
    assert!(archive.insert(b));
    assert!(archive.insert(c));
    assert_eq!(archive.len(), 3);
}

#[test]
fn mismatched_feature_dims_rejected() {
    let mut archive = MapElitesArchive::new(1.0);
    let a = Candidate { text: "a".into(), fitness: 1.0, features: vec![0.5] };
    assert!(archive.insert(a));
    // Now dims is locked at 1. Candidate with 2 features should be rejected.
    let b = Candidate { text: "b".into(), fitness: 2.0, features: vec![0.5, 0.5] };
    assert!(!archive.insert(b));
    assert_eq!(archive.len(), 1);
}

#[test]
fn bucket_index_negative_values() {
    // bucket_index is not pub, so test through archive behaviour.
    let mut archive = MapElitesArchive::new(1.0);
    // -0.5 in bucket -1, 0.5 in bucket 0: different cells.
    let a = Candidate { text: "a".into(), fitness: 1.0, features: vec![-0.5] };
    let b = Candidate { text: "b".into(), fitness: 2.0, features: vec![0.5] };
    assert!(archive.insert(a));
    assert!(archive.insert(b));
    assert_eq!(archive.len(), 2);
}

#[test]
fn elite_insert_sorts_descending() {
    let mut island = Island::new(1.0, 3);
    assert!(island.insert(Candidate { text: "a".into(), fitness: 1.0, features: vec![] }));
    assert!(island.insert(Candidate { text: "b".into(), fitness: 3.0, features: vec![] }));
    assert!(island.insert(Candidate { text: "c".into(), fitness: 2.0, features: vec![] }));
    assert_eq!(island.elites.len(), 3);
    assert_eq!(island.elites[0].text, "b");
    assert_eq!(island.elites[1].text, "c");
    assert_eq!(island.elites[2].text, "a");
}

#[test]
fn elite_truncates_to_k() {
    let mut island = Island::new(1.0, 3);
    for i in 0..5 {
        island.insert(Candidate { text: i.to_string(), fitness: (5 - i) as f64, features: vec![] });
    }
    assert_eq!(island.elites.len(), 3);
    assert_eq!(island.elites[0].fitness, 5.0);
    assert_eq!(island.elites[2].fitness, 3.0);
}

#[test]
fn elite_k_zero_rejects_everything() {
    let mut island = Island::new(1.0, 0);
    assert!(!island.insert(Candidate { text: "a".into(), fitness: 5.0, features: vec![] }));
    assert!(island.elites.is_empty());
}

#[test]
fn elite_insert_past_full_k_rejected() {
    let mut island = Island::new(1.0, 3);
    for i in 0..3 {
        island.insert(Candidate { text: i.to_string(), fitness: (10 - i) as f64, features: vec![] });
    }
    // Full with 10, 9, 8. Inserting 5 should fail.
    assert!(!island.insert(Candidate { text: "weak".into(), fitness: 5.0, features: vec![] }));
    assert_eq!(island.elites.len(), 3);
}

#[test]
fn island_insert_routes_to_archive_when_features_present() {
    let mut island = Island::new(1.0, 3);
    assert!(island.insert(Candidate { text: "a".into(), fitness: 1.0, features: vec![0.5] }));
    assert_eq!(island.archive.len(), 1);
    assert!(island.elites.is_empty());
}

#[test]
fn island_insert_routes_to_elites_when_no_features() {
    let mut island = Island::new(1.0, 3);
    assert!(island.insert(Candidate { text: "a".into(), fitness: 1.0, features: vec![] }));
    assert!(island.archive.is_empty());
    assert_eq!(island.elites.len(), 1);
}

#[test]
fn island_best_picks_across_both() {
    let mut island = Island::new(1.0, 3);
    island.insert(Candidate { text: "archive_lo".into(), fitness: 2.0, features: vec![0.2] });
    island.insert(Candidate { text: "archive_hi".into(), fitness: 4.0, features: vec![0.8] });
    island.insert(Candidate { text: "elite_best".into(), fitness: 5.0, features: vec![] });
    assert_eq!(island.best().unwrap().text, "elite_best");
}

#[test]
fn island_is_empty_and_len() {
    let mut island = Island::new(1.0, 3);
    assert!(island.is_empty());
    assert_eq!(island.len(), 0);
    island.insert(Candidate { text: "a".into(), fitness: 1.0, features: vec![0.2] });
    island.insert(Candidate { text: "b".into(), fitness: 2.0, features: vec![] });
    assert!(!island.is_empty());
    assert_eq!(island.len(), 2);
}
