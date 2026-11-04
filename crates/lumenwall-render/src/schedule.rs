//! Frame operation scheduling (pure): fewer render passes for the same pixels.
//!
//! Images are drawn one after the other: each image's chain renders into its own composites,
//! then its last pass draws into the scene target. In the frame order of spec 04 the scene target
//! therefore changes render pass at every image (37 of the 96 render passes of 3463520581), and
//! every render pass costs CPU in wgpu (≈ 8.5 µs measured) and a GPU barrier. [`schedule`]
//! reorders the operations so that chains run first and scene draws follow each other, while
//! keeping every operation after those it depends on (a write after a read or a write of the
//! same target, a read after a write): the pixels are the same. Chains that read the scene target
//! (refraction, `colorBlendMode`) stay after the scene draws they need.
//!
//! Composites then live from their chain to their scene draw: `budget` bounds the memory of the
//! transient targets kept alive by deferred draws (they cannot share textures while alive).

/// What one frame operation reads and writes, with ids of render targets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpAccess {
    pub reads: Vec<u32>,
    pub writes: Option<u32>,
}

fn conflicts(a: &OpAccess, b: &OpAccess) -> bool {
    let writes_read = |x: &OpAccess, y: &OpAccess| x.writes.is_some_and(|w| y.reads.contains(&w));
    (a.writes.is_some() && a.writes == b.writes) || writes_read(a, b) || writes_read(b, a)
}

/// New order of `ops` (indices into it). `batched(t)`: the target collects draws that should
/// follow each other (the scene target); `transient(t)`: size in bytes of a target that lives
/// only within the frame (composites, effect targets), `None` for the others. A deferred batched
/// draw keeps the transient targets it reads alive; while they exceed `budget` bytes, ready
/// batched draws run first.
pub fn schedule(
    ops: &[OpAccess],
    batched: impl Fn(u32) -> bool,
    transient: impl Fn(u32) -> Option<u64>,
    budget: u64,
) -> Vec<usize> {
    let n = ops.len();
    // Predecessor counts and successor lists (conflicting pairs keep their order).
    let mut preds = vec![0usize; n];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
    for j in 0..n {
        for i in 0..j {
            if conflicts(&ops[i], &ops[j]) {
                preds[j] += 1;
                succs[i].push(j);
            }
        }
    }
    // Remaining accesses of each transient target, to know when it dies.
    let mut remaining: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for op in ops {
        for t in op.reads.iter().chain(op.writes.iter()) {
            if transient(*t).is_some() {
                *remaining.entry(*t).or_default() += 1;
            }
        }
    }
    let mut live: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut live_bytes = 0u64;
    let mut ready: Vec<usize> = (0..n).filter(|&j| preds[j] == 0).collect();
    let mut order = Vec::with_capacity(n);
    let mut current: Option<u32> = None;
    while !ready.is_empty() {
        let is_batched = |j: usize| ops[j].writes.is_some_and(&batched);
        // 1. Continue the render pass in progress.
        let pick = ready
            .iter()
            .copied()
            .filter(|&j| current.is_some() && ops[j].writes == current)
            .min()
            // 2. Over budget: drain with batched draws.
            .or_else(|| {
                (live_bytes > budget)
                    .then(|| ready.iter().copied().filter(|&j| is_batched(j)).min())
                    .flatten()
            })
            // 3. Run chains first; 4. then batched draws.
            .or_else(|| ready.iter().copied().filter(|&j| !is_batched(j)).min())
            .or_else(|| ready.iter().copied().min())
            .expect("ready is not empty");
        ready.retain(|&j| j != pick);
        order.push(pick);
        current = ops[pick].writes;
        for t in ops[pick].reads.iter().chain(ops[pick].writes.iter()) {
            let Some(bytes) = transient(*t) else { continue };
            if live.insert(*t) {
                live_bytes += bytes;
            }
            if let Some(r) = remaining.get_mut(t) {
                *r -= 1;
                if *r == 0 && live.remove(t) {
                    live_bytes -= bytes;
                }
            }
        }
        for &s in &succs[pick] {
            preds[s] -= 1;
            if preds[s] == 0 {
                ready.push(s);
            }
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(reads: &[u32], writes: u32) -> OpAccess {
        OpAccess {
            reads: reads.to_vec(),
            writes: Some(writes),
        }
    }

    /// Render passes of an order: changes of written target.
    fn passes(ops: &[OpAccess], order: &[usize]) -> usize {
        let mut n = 0;
        let mut cur = None;
        for &i in order {
            if ops[i].writes != cur {
                n += 1;
                cur = ops[i].writes;
            }
        }
        n
    }

    /// Every conflicting pair keeps its order.
    fn valid(ops: &[OpAccess], order: &[usize]) -> bool {
        let pos: Vec<usize> = {
            let mut p = vec![0; ops.len()];
            for (k, &i) in order.iter().enumerate() {
                p[i] = k;
            }
            p
        };
        (0..ops.len()).all(|j| (0..j).all(|i| !conflicts(&ops[i], &ops[j]) || pos[i] < pos[j]))
    }

    const SCENE: u32 = 0;

    #[test]
    fn chains_first_then_one_run_of_scene_draws() {
        // Scene clear, then three images: texture -> A_i (10 + i), A_i -> scene.
        let ops = vec![
            op(&[], SCENE),
            op(&[], 11),
            op(&[11], SCENE),
            op(&[], 12),
            op(&[12], SCENE),
            op(&[], 13),
            op(&[13], SCENE),
        ];
        let identity: Vec<usize> = (0..ops.len()).collect();
        assert_eq!(passes(&ops, &identity), 7);
        let order = schedule(
            &ops,
            |t| t == SCENE,
            |t| (t != SCENE).then_some(100),
            u64::MAX,
        );
        assert!(valid(&ops, &order));
        // Three composite passes, then the clear and the three scene draws in one pass.
        assert_eq!(passes(&ops, &order), 4);
        // Draws into the scene keep their order (blending).
        let scene: Vec<usize> = order
            .iter()
            .copied()
            .filter(|&i| ops[i].writes == Some(SCENE))
            .collect();
        assert_eq!(scene, vec![0, 2, 4, 6]);
    }

    #[test]
    fn a_chain_reading_the_scene_waits_for_the_draws_before_it() {
        // Image 2's chain samples the scene (refraction): it must see image 1's draw.
        let ops = vec![
            op(&[], 11),
            op(&[11], SCENE),
            op(&[SCENE], 12),
            op(&[12], SCENE),
            op(&[], 13),
            op(&[13], SCENE),
        ];
        let order = schedule(
            &ops,
            |t| t == SCENE,
            |t| (t != SCENE).then_some(100),
            u64::MAX,
        );
        assert!(valid(&ops, &order));
        let pos = |i: usize| order.iter().position(|&x| x == i).unwrap();
        assert!(pos(1) < pos(2));
        // Image 3's chain needs nothing: it moves before image 2's scene draw run.
        assert!(pos(4) < pos(3));
    }

    #[test]
    fn the_budget_bounds_composites_kept_alive() {
        // Four images whose composites (100 bytes each) live until their scene draw.
        let mut ops = vec![op(&[], SCENE)];
        for i in 0..4 {
            ops.push(op(&[], 11 + i));
            ops.push(op(&[11 + i], SCENE));
        }
        let all = schedule(
            &ops,
            |t| t == SCENE,
            |t| (t != SCENE).then_some(100),
            u64::MAX,
        );
        let tight = schedule(&ops, |t| t == SCENE, |t| (t != SCENE).then_some(100), 150);
        assert!(valid(&ops, &all) && valid(&ops, &tight));
        assert!(passes(&ops, &tight) > passes(&ops, &all));
        // Never more than two composites alive at once with a 150-byte budget.
        let mut alive = 0usize;
        let mut max_alive = 0;
        for &i in &tight {
            if ops[i].writes != Some(SCENE) {
                alive += 1;
            } else if !ops[i].reads.is_empty() {
                alive -= 1;
            }
            max_alive = max_alive.max(alive);
        }
        assert!(max_alive <= 2, "{max_alive}");
    }

    #[test]
    fn writes_to_the_same_target_keep_their_order() {
        let ops = vec![op(&[], 5), op(&[], 6), op(&[], 5), op(&[5], 6)];
        let order = schedule(&ops, |t| t == 6, |_| Some(1), u64::MAX);
        assert!(valid(&ops, &order));
        assert_eq!(order.len(), 4);
    }
}
