//! Tests for `crates/deepseek-custom/src/evolution/mod.rs`.
//!
//! Pure unit tests against hand-built fitness numbers. No `StubBackend`
//! needed here: every function this module covers takes plain values in
//! and returns plain values out.

use deepseek_custom::evolution::*;

#[test]
fn candidate_no_features_is_not_archived() {
    let mut archive = MapElitesArchive::new(0.5);
    let c = Candidate {
        text: "a".into(),
        fitness: 1.0,
        features: vec![],
    };
    assert!(!archive.insert(c));
    assert!(archive.is_empty());
}

#[test]
fn first_candidate_wins_empty_cell() {
    let mut archive = MapElitesArchive::new(0.5);
    let c = Candidate {
        text: "a".into(),
        fitness: 1.0,
        features: vec![0.2],
    };
    assert!(archive.insert(c));
    assert_eq!(archive.len(), 1);
}

#[test]
fn higher_fitness_replaces_lower_in_same_cell() {
    let mut archive = MapElitesArchive::new(0.5);
    let lo = Candidate {
        text: "lo".into(),
        fitness: 1.0,
        features: vec![0.2],
    };
    let hi = Candidate {
        text: "hi".into(),
        fitness: 3.0,
        features: vec![0.3],
    };
    // 0.2/0.5 -> bucket 0, 0.3/0.5 -> bucket 0: same cell.
    assert!(archive.insert(lo));
    assert!(archive.insert(hi));
    assert_eq!(archive.len(), 1);
    assert_eq!(archive.best().unwrap().text, "hi");
}

#[test]
fn lower_fitness_loses_to_existing() {
    let mut archive = MapElitesArchive::new(0.5);
    let hi = Candidate {
        text: "hi".into(),
        fitness: 3.0,
        features: vec![0.2],
    };
    let lo = Candidate {
        text: "lo".into(),
        fitness: 1.0,
        features: vec![0.3],
    };
    assert!(archive.insert(hi));
    assert!(!archive.insert(lo));
    assert_eq!(archive.best().unwrap().text, "hi");
}

#[test]
fn different_cells_keep_both() {
    let mut archive = MapElitesArchive::new(1.0);
    let a = Candidate {
        text: "a".into(),
        fitness: 5.0,
        features: vec![0.5],
    }; // bucket 0
    let b = Candidate {
        text: "b".into(),
        fitness: 2.0,
        features: vec![1.5],
    }; // bucket 1
    assert!(archive.insert(a));
    assert!(archive.insert(b));
    assert_eq!(archive.len(), 2);
}

#[test]
fn two_dimensional_grid() {
    let mut archive = MapElitesArchive::new(1.0);
    let a = Candidate {
        text: "a".into(),
        fitness: 1.0,
        features: vec![0.5, 0.5],
    };
    let b = Candidate {
        text: "b".into(),
        fitness: 2.0,
        features: vec![0.5, 1.5],
    };
    let c = Candidate {
        text: "c".into(),
        fitness: 3.0,
        features: vec![1.5, 0.5],
    };
    assert!(archive.insert(a));
    assert!(archive.insert(b));
    assert!(archive.insert(c));
    assert_eq!(archive.len(), 3);
}

#[test]
fn mismatched_feature_dims_rejected() {
    let mut archive = MapElitesArchive::new(1.0);
    let a = Candidate {
        text: "a".into(),
        fitness: 1.0,
        features: vec![0.5],
    };
    assert!(archive.insert(a));
    // Now dims is locked at 1. Candidate with 2 features should be rejected.
    let b = Candidate {
        text: "b".into(),
        fitness: 2.0,
        features: vec![0.5, 0.5],
    };
    assert!(!archive.insert(b));
    assert_eq!(archive.len(), 1);
}

#[test]
fn bucket_index_negative_values() {
    // bucket_index is not pub, so test through archive behaviour.
    let mut archive = MapElitesArchive::new(1.0);
    // -0.5 in bucket -1, 0.5 in bucket 0: different cells.
    let a = Candidate {
        text: "a".into(),
        fitness: 1.0,
        features: vec![-0.5],
    };
    let b = Candidate {
        text: "b".into(),
        fitness: 2.0,
        features: vec![0.5],
    };
    assert!(archive.insert(a));
    assert!(archive.insert(b));
    assert_eq!(archive.len(), 2);
}

#[test]
fn elite_insert_sorts_descending() {
    let mut island = Island::new(1.0, 3);
    assert!(island.insert(Candidate {
        text: "a".into(),
        fitness: 1.0,
        features: vec![]
    }));
    assert!(island.insert(Candidate {
        text: "b".into(),
        fitness: 3.0,
        features: vec![]
    }));
    assert!(island.insert(Candidate {
        text: "c".into(),
        fitness: 2.0,
        features: vec![]
    }));
    assert_eq!(island.elites.len(), 3);
    assert_eq!(island.elites[0].text, "b");
    assert_eq!(island.elites[1].text, "c");
    assert_eq!(island.elites[2].text, "a");
}

#[test]
fn elite_truncates_to_k() {
    let mut island = Island::new(1.0, 3);
    for i in 0..5 {
        island.insert(Candidate {
            text: i.to_string(),
            fitness: (5 - i) as f64,
            features: vec![],
        });
    }
    assert_eq!(island.elites.len(), 3);
    assert_eq!(island.elites[0].fitness, 5.0);
    assert_eq!(island.elites[2].fitness, 3.0);
}

#[test]
fn elite_k_zero_rejects_everything() {
    let mut island = Island::new(1.0, 0);
    assert!(!island.insert(Candidate {
        text: "a".into(),
        fitness: 5.0,
        features: vec![]
    }));
    assert!(island.elites.is_empty());
}

#[test]
fn elite_insert_past_full_k_rejected() {
    let mut island = Island::new(1.0, 3);
    for i in 0..3 {
        island.insert(Candidate {
            text: i.to_string(),
            fitness: (10 - i) as f64,
            features: vec![],
        });
    }
    // Full with 10, 9, 8. Inserting 5 should fail.
    assert!(!island.insert(Candidate {
        text: "weak".into(),
        fitness: 5.0,
        features: vec![]
    }));
    assert_eq!(island.elites.len(), 3);
}

#[test]
fn island_insert_routes_to_archive_when_features_present() {
    let mut island = Island::new(1.0, 3);
    assert!(island.insert(Candidate {
        text: "a".into(),
        fitness: 1.0,
        features: vec![0.5]
    }));
    assert_eq!(island.archive.len(), 1);
    assert!(island.elites.is_empty());
}

#[test]
fn island_insert_routes_to_elites_when_no_features() {
    let mut island = Island::new(1.0, 3);
    assert!(island.insert(Candidate {
        text: "a".into(),
        fitness: 1.0,
        features: vec![]
    }));
    assert!(island.archive.is_empty());
    assert_eq!(island.elites.len(), 1);
}

#[test]
fn island_best_picks_across_both() {
    let mut island = Island::new(1.0, 3);
    island.insert(Candidate {
        text: "archive_lo".into(),
        fitness: 2.0,
        features: vec![0.2],
    });
    island.insert(Candidate {
        text: "archive_hi".into(),
        fitness: 4.0,
        features: vec![0.8],
    });
    island.insert(Candidate {
        text: "elite_best".into(),
        fitness: 5.0,
        features: vec![],
    });
    assert_eq!(island.best().unwrap().text, "elite_best");
}

#[test]
fn island_is_empty_and_len() {
    let mut island = Island::new(1.0, 3);
    assert!(island.is_empty());
    assert_eq!(island.len(), 0);
    island.insert(Candidate {
        text: "a".into(),
        fitness: 1.0,
        features: vec![0.2],
    });
    island.insert(Candidate {
        text: "b".into(),
        fitness: 2.0,
        features: vec![],
    });
    assert!(!island.is_empty());
    assert_eq!(island.len(), 2);
}

// --- select_parent ---

#[test]
fn select_parent_empty_island_returns_none() {
    let island = Island::new(1.0, 3);
    assert!(island.select_parent(0).is_none());
}

#[test]
fn select_parent_round_robins_archive() {
    let mut island = Island::new(1.0, 3);
    // Three cells, different bucket keys.
    island.insert(Candidate {
        text: "cell0".into(),
        fitness: 1.0,
        features: vec![0.2],
    }); // bucket 0
    island.insert(Candidate {
        text: "cell1".into(),
        fitness: 2.0,
        features: vec![1.2],
    }); // bucket 1
    island.insert(Candidate {
        text: "cell2".into(),
        fitness: 3.0,
        features: vec![2.2],
    }); // bucket 2
    // Sorted keys: [0], [1], [2] -> cell0, cell1, cell2.
    assert_eq!(island.select_parent(0).unwrap().text, "cell0");
    assert_eq!(island.select_parent(1).unwrap().text, "cell1");
    assert_eq!(island.select_parent(2).unwrap().text, "cell2");
    // Wraps around.
    assert_eq!(island.select_parent(3).unwrap().text, "cell0");
}

#[test]
fn select_parent_round_robins_elites() {
    let mut island = Island::new(1.0, 3);
    island.insert(Candidate {
        text: "a".into(),
        fitness: 3.0,
        features: vec![],
    });
    island.insert(Candidate {
        text: "b".into(),
        fitness: 2.0,
        features: vec![],
    });
    island.insert(Candidate {
        text: "c".into(),
        fitness: 1.0,
        features: vec![],
    });
    // elites is sorted descending by fitness: a(3), b(2), c(1).
    assert_eq!(island.select_parent(0).unwrap().text, "a");
    assert_eq!(island.select_parent(1).unwrap().text, "b");
    assert_eq!(island.select_parent(2).unwrap().text, "c");
    assert_eq!(island.select_parent(3).unwrap().text, "a");
}

#[test]
fn select_parent_prefers_archive_over_elites() {
    let mut island = Island::new(1.0, 3);
    // Both archive and elites populated.
    island.insert(Candidate {
        text: "arch".into(),
        fitness: 1.0,
        features: vec![0.5],
    });
    island.insert(Candidate {
        text: "elite_best".into(),
        fitness: 99.0,
        features: vec![],
    });
    // Should pick from archive even though elite has higher fitness.
    assert_eq!(island.select_parent(0).unwrap().text, "arch");
}

#[test]
fn select_parent_deterministic() {
    let mut island = Island::new(1.0, 3);
    island.insert(Candidate {
        text: "x".into(),
        fitness: 1.0,
        features: vec![0.5],
    });
    island.insert(Candidate {
        text: "y".into(),
        fitness: 2.0,
        features: vec![1.5],
    });
    // Same state, same round -> same result every time.
    let first = island.select_parent(7).unwrap().text.clone();
    for _ in 0..10 {
        assert_eq!(island.select_parent(7).unwrap().text, first);
    }
}

// --- migrate ---

#[test]
fn migrate_noop_with_fewer_than_two_islands() {
    let mut islands = vec![Island::new(1.0, 3)];
    islands[0].insert(Candidate {
        text: "a".into(),
        fitness: 5.0,
        features: vec![0.5],
    });
    let before = islands[0].len();
    migrate(&mut islands);
    // Nothing changes: fewer than 2 islands.
    assert_eq!(islands.len(), 1);
    assert_eq!(islands[0].len(), before);
    assert_eq!(islands[0].best().unwrap().text, "a");
}

#[test]
fn migrate_noop_when_no_candidates() {
    let mut islands = vec![
        Island::new(1.0, 3),
        Island::new(1.0, 3),
        Island::new(1.0, 3),
        Island::new(1.0, 3),
    ];
    let snapshot: Vec<usize> = islands.iter().map(|i| i.len()).collect();
    migrate(&mut islands);
    // No candidate anywhere: every island untouched.
    for (idx, isle) in islands.iter().enumerate() {
        assert_eq!(isle.len(), snapshot[idx]);
    }
}

#[test]
fn migrate_resets_bottom_half() {
    // 4 islands: ranked by best fitness, bottom 2 reset.
    let mut islands = vec![
        Island::new(1.0, 3), // idx 0: best fitness 1.0 (bottom half)
        Island::new(1.0, 3), // idx 1: best fitness 3.0 (top half)
        Island::new(1.0, 3), // idx 2: best fitness 2.0 (bottom half)
        Island::new(1.0, 3), // idx 3: best fitness 5.0 (top half, global best)
    ];
    islands[0].insert(Candidate {
        text: "lo".into(),
        fitness: 1.0,
        features: vec![0.5],
    });
    islands[1].insert(Candidate {
        text: "mid".into(),
        fitness: 3.0,
        features: vec![0.5],
    });
    islands[2].insert(Candidate {
        text: "mid2".into(),
        fitness: 2.0,
        features: vec![0.5],
    });
    islands[3].insert(Candidate {
        text: "hi".into(),
        fitness: 5.0,
        features: vec![0.5],
    });

    migrate(&mut islands);

    // Top half (indices 1 and 3, best 3.0 and 5.0) untouched.
    assert_eq!(islands[1].best().unwrap().text, "mid");
    assert_eq!(islands[3].best().unwrap().text, "hi");

    // Bottom half (indices 0 and 2) reset and reseeded with global best.
    assert_eq!(islands[0].best().unwrap().text, "hi");
    assert_eq!(islands[2].best().unwrap().text, "hi");
}

#[test]
fn migrate_reseeds_with_global_best_clone() {
    let mut islands = vec![Island::new(1.0, 3), Island::new(1.0, 3)];
    islands[0].insert(Candidate {
        text: "champ".into(),
        fitness: 10.0,
        features: vec![0.5],
    });
    islands[1].insert(Candidate {
        text: "loser".into(),
        fitness: 1.0,
        features: vec![0.5],
    });

    migrate(&mut islands);

    // Top half (idx 0, best 10.0) untouched.
    assert_eq!(islands[0].len(), 1);
    assert_eq!(islands[0].best().unwrap().text, "champ");

    // Bottom half (idx 1) reset, holds one clone of the global best.
    assert_eq!(islands[1].len(), 1);
    assert_eq!(islands[1].best().unwrap().text, "champ");
    // Verify clone independence: inserting into a different cell of
    // island[1] leaves island[0] unchanged.
    islands[1].insert(Candidate {
        text: "newcomer".into(),
        fitness: 0.5,
        features: vec![9.0],
    });
    assert_eq!(islands[0].len(), 1); // idx 0 unchanged.
    assert_eq!(islands[1].len(), 2); // idx 1 now has two cells.
}

#[test]
fn migrate_handles_empty_islands_in_ranking() {
    // One empty island (no candidate) and one populated island.
    let mut islands = vec![
        Island::new(1.0, 3), // empty
        Island::new(1.0, 3), // populated
    ];
    islands[1].insert(Candidate {
        text: "sole".into(),
        fitness: 3.0,
        features: vec![0.5],
    });

    migrate(&mut islands);

    // Top half: idx 1 (populated, best 3.0) untouched.
    assert_eq!(islands[1].best().unwrap().text, "sole");
    // Bottom half: idx 0 (empty, ranked last) reset and reseeded.
    assert_eq!(islands[0].best().unwrap().text, "sole");
}

#[test]
fn migrate_with_elite_fallback_islands() {
    // Two islands with no features (elite fallback).
    let mut islands = vec![Island::new(1.0, 3), Island::new(1.0, 3)];
    islands[0].insert(Candidate {
        text: "alpha".into(),
        fitness: 5.0,
        features: vec![],
    });
    islands[1].insert(Candidate {
        text: "beta".into(),
        fitness: 2.0,
        features: vec![],
    });

    migrate(&mut islands);

    // Top half (idx 0, best 5.0) untouched.
    assert_eq!(islands[0].best().unwrap().text, "alpha");
    // Bottom half (idx 1) reset, holds global best.
    assert_eq!(islands[1].best().unwrap().text, "alpha");
}

#[test]
fn migrate_odd_island_count_resets_floor_half() {
    // 5 islands: bottom 2 reset (5 / 2 = 2).
    let mut islands = vec![
        Island::new(1.0, 3), // fitness 1.0
        Island::new(1.0, 3), // fitness 2.0
        Island::new(1.0, 3), // fitness 3.0
        Island::new(1.0, 3), // fitness 4.0
        Island::new(1.0, 3), // fitness 5.0 (global best)
    ];
    for (i, isle) in islands.iter_mut().enumerate() {
        isle.insert(Candidate {
            text: format!("c{}", i),
            fitness: (i + 1) as f64,
            features: vec![0.5],
        });
    }

    migrate(&mut islands);

    // Top 3 islands (best 3.0, 4.0, 5.0) untouched.
    assert_eq!(islands[2].best().unwrap().text, "c2");
    assert_eq!(islands[3].best().unwrap().text, "c3");
    assert_eq!(islands[4].best().unwrap().text, "c4");

    // Bottom 2 (best 1.0, 2.0) reset and reseeded with global best "c4".
    assert_eq!(islands[0].best().unwrap().text, "c4");
    assert_eq!(islands[1].best().unwrap().text, "c4");
}

#[test]
fn migrate_global_best_determines_reseed() {
    // The global best candidate should win regardless of island index.
    let mut islands = vec![
        Island::new(1.0, 3),
        Island::new(1.0, 3),
        Island::new(1.0, 3),
    ];
    // idx 0: low, idx 1: highest, idx 2: medium.
    islands[0].insert(Candidate {
        text: "low".into(),
        fitness: 1.0,
        features: vec![0.5],
    });
    islands[1].insert(Candidate {
        text: "best_overall".into(),
        fitness: 9.0,
        features: vec![0.5],
    });
    islands[2].insert(Candidate {
        text: "mid".into(),
        fitness: 5.0,
        features: vec![0.5],
    });

    migrate(&mut islands);
    // 3 islands: bottom 1 resets (3 / 2 = 1). Bottom is idx 0 (best 1.0).
    // Reseeded with global best "best_overall".
    assert_eq!(islands[0].best().unwrap().text, "best_overall");
    assert_eq!(islands[1].best().unwrap().text, "best_overall");
    assert_eq!(islands[2].best().unwrap().text, "mid");
}
