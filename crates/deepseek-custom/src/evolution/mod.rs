//! Programmatic evolutionary search: population archive and selection.
//!
//! `Api`-only.  Holds every decision that must never depend on a model:
//! which candidates survive, which parent breeds next, and when an island
//! resets.  The dispatch layer in `src/search/evolve/mod.rs` handles the one piece
//! that a model owns (writing each new candidate's text), and calls into
//! this module.  Nothing here touches the network, a child process, or a
//! model call.

use std::collections::HashMap;

/// One generated answer with a fitness score and optional behavioural
/// coordinates.
#[derive(Clone, Debug)]
pub struct Candidate {
    /// The raw generated text.
    pub text: String,
    /// Scalar fitness value, higher is better.  Produced by `fitness_cmd`.
    pub fitness: f64,
    /// Optional feature vector, one dimension per behavioural axis.
    /// Produced by `feature_cmd`.  When present this drives the
    /// [`MapElitesArchive`] grid; absent, the island falls back to plain
    /// top-`k` fitness elitism.
    pub features: Vec<f64>,
}

/// A fixed-width grid archive that keeps the best [`Candidate`] per cell.
///
/// Each feature dimension is discretized into equal-width buckets.  A
/// candidate's cell key is the vector of bucket indices across every
/// dimension.  `insert` keeps the higher-fitness candidate when two
/// candidates fall into the same cell.
///
/// The grid is sparse: cells are only allocated when a candidate lands in
/// them.
#[derive(Clone, Debug)]
pub struct MapElitesArchive {
    /// Bucket width per dimension.  Every dimension shares one width
    /// because the archive has no prior knowledge of feature ranges; the
    /// first insertion seeds each dimension's range and the width is
    /// applied from then on.
    pub bucket_width: f64,

    /// Occupied cells.  Key is `Vec<isize>` (the bucket index per
    /// dimension), value is the best candidate for that cell so far.
    cells: HashMap<Vec<isize>, Candidate>,

    /// The number of feature dimensions this archive was initialized for.
    /// Set on first insertion (or known at construction), unchanged after.
    dims: usize,
}

impl MapElitesArchive {
    /// Create an empty archive with the given bucket width.
    pub fn new(bucket_width: f64) -> Self {
        Self {
            bucket_width,
            cells: HashMap::new(),
            dims: 0,
        }
    }

    /// Number of occupied cells.
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// `true` when no cell has been filled yet.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Iterate over occupied cells, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = (&[isize], &Candidate)> {
        self.cells.iter().map(|(k, v)| (k.as_slice(), v))
    }

    /// Insert `candidate` into the archive.
    ///
    /// Returns `true` when the candidate won its cell (it was empty, or
    /// the new fitness beat the old one).  Returns `false` when it lost.
    ///
    /// When `candidate.features` is empty, insertion is a no-op: this
    /// archive requires a feature vector.  Callers that have no features
    /// should use plain top-`k` elitism instead.
    pub fn insert(&mut self, candidate: Candidate) -> bool {
        let features = &candidate.features;
        if features.is_empty() {
            return false;
        }

        // Lock dimensions on first insertion.
        if self.dims == 0 {
            self.dims = features.len();
        } else if features.len() != self.dims {
            // Mismatched feature count: skip (caller should never do
            // this, but the archive must not panic on it).
            return false;
        }

        let key: Vec<isize> = features
            .iter()
            .map(|f| bucket_index(*f, self.bucket_width))
            .collect();

        match self.cells.get(&key) {
            Some(existing) if existing.fitness >= candidate.fitness => false,
            _ => {
                self.cells.insert(key, candidate);
                true
            }
        }
    }

    /// Return the single highest-fitness candidate across the archive,
    /// or `None` when empty.
    pub fn best(&self) -> Option<&Candidate> {
        self.cells
            .values()
            .max_by(|a, b| a.fitness.total_cmp(&b.fitness))
    }
}

/// Map a feature value to a bucket index using fixed-width discretization.
fn bucket_index(value: f64, width: f64) -> isize {
    let w = if width <= 0.0 { 1.0 } else { width };
    (value / w).floor() as isize
}

/// One isolated sub-population with its own [`MapElitesArchive`].
///
/// Without a feature vector an island falls back to plain top-`k` fitness
/// elitism instead of a grid.  That path is not implemented in
/// `MapElitesArchive` itself; callers that have no features keep a
/// separate `Vec<Candidate>`.
#[derive(Clone, Debug)]
pub struct Island {
    /// The MAP-Elites archive for candidates that carry a feature vector.
    /// Empty (0 dims) when no feature command is configured.
    pub archive: MapElitesArchive,

    /// Plain elitism fallback: best `k` candidates by fitness, kept when
    /// no feature vector is available.  Truncated after each insertion to
    /// `elite_k`.
    pub elites: Vec<Candidate>,

    /// Maximum number of elites to keep in the fallback list.
    pub elite_k: usize,
}

impl Island {
    /// Create a fresh island.
    pub fn new(bucket_width: f64, elite_k: usize) -> Self {
        Self {
            archive: MapElitesArchive::new(bucket_width),
            elites: Vec::new(),
            elite_k,
        }
    }

    /// Insert a candidate into whichever half of the island it belongs
    /// in (archive when it carries features, elite list otherwise).
    /// Returns `true` when the candidate was kept.
    pub fn insert(&mut self, candidate: Candidate) -> bool {
        if candidate.features.is_empty() {
            insert_elite(&mut self.elites, self.elite_k, candidate)
        } else {
            self.archive.insert(candidate)
        }
    }

    /// The single best candidate across both the archive and the elite
    /// fallback, or `None` when the island is empty.
    pub fn best(&self) -> Option<&Candidate> {
        let archive_best = self.archive.best();
        let elite_best = self
            .elites
            .iter()
            .max_by(|a, b| a.fitness.total_cmp(&b.fitness));
        match (archive_best, elite_best) {
            (Some(a), Some(e)) => {
                if a.fitness >= e.fitness {
                    Some(a)
                } else {
                    Some(e)
                }
            }
            (Some(a), None) => Some(a),
            (None, Some(e)) => Some(e),
            (None, None) => None,
        }
    }

    /// Select a parent candidate deterministically by round-robining
    /// through occupied cells.  `round` is a generation counter or any
    /// monotonic integer; successive calls with successive values cycle
    /// through different cells rather than always returning the single
    /// best.
    ///
    /// Prefers archive cells when the archive is non-empty, otherwise
    /// falls back to the elite list.  Cells are visited in a fixed
    /// order (sorted by key for determinism), so the same `round`
    /// always picks the same candidate from the same island state.
    ///
    /// Returns `None` when the island is empty.
    pub fn select_parent(&self, round: usize) -> Option<&Candidate> {
        if !self.archive.is_empty() {
            let mut cells: Vec<(&[isize], &Candidate)> = self.archive.iter().collect();
            cells.sort_by(|(key_a, _), (key_b, _)| key_a.cmp(key_b));
            Some(cells[round % cells.len()].1)
        } else if !self.elites.is_empty() {
            Some(&self.elites[round % self.elites.len()])
        } else {
            None
        }
    }

    /// `true` when the island has no candidates at all.
    pub fn is_empty(&self) -> bool {
        self.archive.is_empty() && self.elites.is_empty()
    }

    /// Total number of candidates held in this island.
    pub fn len(&self) -> usize {
        self.archive.len() + self.elites.len()
    }
}

/// Insert `candidate` into a sorted elite list, truncating to `k`.
/// The list is kept in descending fitness order.
/// Returns `true` when the candidate was kept.
fn insert_elite(elites: &mut Vec<Candidate>, k: usize, candidate: Candidate) -> bool {
    if k == 0 {
        return false;
    }

    // Find insertion point (descending fitness).
    let pos = elites.partition_point(|c| c.fitness > candidate.fitness);

    // If the list is full and we would be inserting past the end, reject.
    if pos >= k {
        return false;
    }

    elites.insert(pos, candidate);
    elites.truncate(k);
    true
}

/// Run every `migration_interval` rounds.
///
/// Ranks islands by their best candidate's fitness (descending).  Resets the
/// bottom half: clears their archive and elite list, then reseeds each with a
/// clone of the single best candidate found across every island.  This is
/// FunSearch's own rule, reimplemented here in Rust instead of left to a tool
/// call.
///
/// A no-op when there are fewer than 2 islands, or when no island has a
/// candidate.
pub fn migrate(islands: &mut [Island]) {
    let n = islands.len();
    if n < 2 {
        return;
    }

    // Find the single best candidate across every island by index so no
    // reference into `islands` outlives this block.
    let mut best_fitness: Option<f64> = None;
    let mut best_idx: Option<usize> = None;
    for (idx, isle) in islands.iter().enumerate() {
        if let Some(c) = isle.best()
            && best_fitness.is_none_or(|bf| c.fitness > bf)
        {
            best_fitness = Some(c.fitness);
            best_idx = Some(idx);
        }
    }
    let Some(best_idx) = best_idx else {
        return; // No candidate anywhere.
    };
    let global_best = islands[best_idx].best().unwrap().clone();

    // Rank islands by their best fitness, descending.  Islands with no
    // candidate sort to the end.
    let mut ranked: Vec<(usize, Option<f64>)> = islands
        .iter()
        .enumerate()
        .map(|(idx, isle)| (idx, isle.best().map(|c| c.fitness)))
        .collect();
    ranked.sort_by(|(_, fa), (_, fb)| {
        // None sorts after Some (descending: Some first, None last).
        match (fa, fb) {
            (Some(a), Some(b)) => b.total_cmp(a), // descending
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });

    // Reset the bottom half.
    let bottom_count = n / 2;
    for &(idx, _) in &ranked[n - bottom_count..] {
        let island = &mut islands[idx];
        island.archive = MapElitesArchive::new(island.archive.bucket_width);
        island.elites.clear();
        island.insert(global_best.clone());
    }
}
