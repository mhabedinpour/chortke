use slab::Slab;
use std::collections::{HashMap, HashSet};
use std::ops::{Index, IndexMut};

/// A thin wrapper around `slab::Slab` that can take a point-in-time snapshot.
///
/// While a snapshot is active:
/// - The first time a preexisting key is modified (via `modify`, `IndexMut`, or `remove`),
///   its old value is stashed.
/// - `get_for_snapshot` returns the stashed old value for such keys, allowing readers to see a
///   consistent view that corresponds to the moment `begin_snapshot` was called.
/// - Keys inserted after `begin_snapshot` are NOT part of the snapshot, so they are never stashed
///   and `get_for_snapshot` returns their current value (or `None` once removed).
///
/// Only one snapshot can be active at a time.
#[derive(Debug)]
pub struct SnapshotableSlab<T> {
    slab: Slab<T>,
    snap: Option<SnapshotCtx<T>>,
    live_keys: HashSet<usize>,
}

#[derive(Debug)]
struct SnapshotCtx<T> {
    /// Keys inserted after the snapshot was started.
    keys_after: HashSet<usize>,
    /// Old values for keys that have been changed/removed since the snapshot began.
    old: HashMap<usize, T>,
}

impl<T> Default for SnapshotableSlab<T> {
    fn default() -> Self {
        Self {
            slab: Slab::new(),
            snap: None,
            live_keys: HashSet::new(),
        }
    }
}

impl<T: Clone> SnapshotableSlab<T> {
    /// Begins a snapshot and returns a vector of the key list that existed at that moment.
    ///
    /// Panics if a snapshot is already active.
    pub fn begin_snapshot(&mut self) -> Vec<usize> {
        assert!(self.snap.is_none());
        let ctx = SnapshotCtx {
            keys_after: HashSet::new(),
            old: HashMap::new(),
        };
        self.snap = Some(ctx);
        self.live_keys.iter().copied().collect()
    }

    /// Ends the current snapshot (if any), returning to normal semantics.
    pub fn end_snapshot(&mut self) {
        self.snap = None;
    }

    /// If a snapshot is active and `key` was present at snapshot start, stash old value once.
    fn maybe_stash_old(&mut self, key: usize) {
        if let Some(ctx) = &mut self.snap {
            if ctx.old.contains_key(&key) {
                return;
            }
            if !ctx.keys_after.contains(&key) {
                let old = self.slab[key].clone();
                ctx.old.insert(key, old);
            }
        }
    }

    /// Inserts a value into the underlying slab and returns its key.
    pub fn insert(&mut self, v: T) -> usize {
        let k = self.slab.insert(v);
        if let Some(snap) = &mut self.snap {
            snap.keys_after.insert(k);
        }
        self.live_keys.insert(k);
        k
    }

    /// Gets the current value for `key`, regardless of snapshot state.
    pub fn get(&self, key: usize) -> Option<&T> {
        self.slab.get(key)
    }

    /// Gets a value suitable for readers during a snapshot.
    ///
    /// - If a snapshot is active and an old value was stashed for `key`, that old value is returned.
    /// - Otherwise, returns the current value from the slab.
    pub fn get_for_snapshot(&self, key: usize) -> Option<&T> {
        match &self.snap {
            Some(ctx) => {
                if let Some(old) = ctx.old.get(&key) {
                    return Some(old);
                }
                self.slab.get(key)
            }
            None => self.slab.get(key),
        }
    }

    /// Mutates an entry by key using the provided closure. No-op if the key doesn't exist.
    pub fn modify<F: FnOnce(&mut T)>(&mut self, key: usize, f: F) {
        self.maybe_stash_old(key);
        if let Some(v) = self.slab.get_mut(key) {
            f(v);
        }
    }

    /// Removes and returns the current value at `key`.
    /// If a snapshot is active and `key` was present at snapshot start, the old value is stashed first.
    pub fn remove(&mut self, key: usize) -> T {
        self.maybe_stash_old(key);
        let v = self.slab.remove(key);
        if let Some(snap) = &mut self.snap {
            snap.keys_after.remove(&key);
        }
        self.live_keys.remove(&key);
        v
    }
}

impl<T> Index<usize> for SnapshotableSlab<T> {
    type Output = T;

    fn index(&self, key: usize) -> &Self::Output {
        &self.slab[key]
    }
}

impl<T: Clone> IndexMut<usize> for SnapshotableSlab<T> {
    fn index_mut(&mut self, key: usize) -> &mut Self::Output {
        self.maybe_stash_old(key);

        &mut self.slab[key]
    }
}

#[cfg(test)]
mod tests {
    use super::SnapshotableSlab;

    #[test]
    fn begin_and_end_snapshot() {
        let mut s: SnapshotableSlab<i32> = SnapshotableSlab::default();
        let a = s.insert(10);
        let b = s.insert(20);
        let mut ks = s.begin_snapshot();
        ks.sort_unstable();
        let mut expected = vec![a, b];
        expected.sort_unstable();
        assert_eq!(
            ks, expected,
            "keys after begin_snapshot mismatch: got {:?}, expected {:?}",
            ks, expected
        );

        // cannot begin another snapshot
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = s.begin_snapshot();
        }));
        assert!(
            result.is_err(),
            "begin_snapshot should fail when a snapshot is already active"
        );

        // ending snapshot restores normal behavior
        s.end_snapshot();
        assert_eq!(
            s.get_for_snapshot(a),
            Some(&10),
            "after end_snapshot, snapshot view should equal current for key a"
        );
    }

    #[test]
    fn get_for_snapshot_returns_old_on_modify_and_remove() {
        let mut s: SnapshotableSlab<i32> = SnapshotableSlab::default();
        let a = s.insert(1);
        let b = s.insert(2);
        let _keys = s.begin_snapshot();

        // modify a via modify
        s.modify(a, |v| *v = 11);
        // snapshot view should still see the old value
        assert_eq!(
            s.get_for_snapshot(a),
            Some(&1),
            "snapshot view should return stashed old value for modified key a"
        );
        // current view sees new value
        assert_eq!(
            s.get(a),
            Some(&11),
            "current view should reflect modified value for key a"
        );

        // remove b
        let removed = s.remove(b);
        assert_eq!(
            removed, 2,
            "remove should return the current value for key b"
        );
        // snapshot view can still see old b
        assert_eq!(
            s.get_for_snapshot(b),
            Some(&2),
            "snapshot view should return stashed old value for removed key b"
        );
        // current view sees it's gone
        assert!(
            s.get(b).is_none(),
            "current view should not contain key b after removal"
        );

        // after ending snapshot, snapshot view equals current
        s.end_snapshot();
        assert_eq!(
            s.get_for_snapshot(a),
            Some(&11),
            "after end_snapshot, snapshot view should reflect current value for key a"
        );
        assert_eq!(
            s.get_for_snapshot(b),
            None,
            "after end_snapshot, removed key b should be absent in snapshot view as well"
        );
    }

    #[test]
    fn new_insertions_are_not_stashed() {
        let mut s: SnapshotableSlab<i32> = SnapshotableSlab::default();
        let _ = s.begin_snapshot();

        let k = s.insert(7);
        // modify new key; since it wasn't present at snapshot start, there is no old stash
        s.modify(k, |v| *v = 8);
        assert_eq!(
            s.get_for_snapshot(k),
            Some(&8),
            "newly inserted key k should not be stashed; snapshot view shows latest value"
        );

        // removing also should not produce an old value for snapshot
        let _ = s.remove(k);
        assert_eq!(
            s.get_for_snapshot(k),
            None,
            "newly inserted key k should not appear in snapshot view after removal"
        );
    }

    #[test]
    fn index_mut_triggers_stash() {
        let mut s: SnapshotableSlab<String> = SnapshotableSlab::default();
        let k = s.insert("a".to_string());
        let _ = s.begin_snapshot();

        // mutate via IndexMut
        s[k].push('b');

        // snapshot should return old value
        assert_eq!(
            s.get_for_snapshot(k).map(|v| v.as_str()),
            Some("a"),
            "index_mut should stash old value; snapshot view must still return \"a\""
        );
        // current returns new value
        assert_eq!(
            s.get(k).map(|v| v.as_str()),
            Some("ab"),
            "current view should show mutated string 'ab'"
        );
    }

    #[test]
    fn modify_noop_for_missing_key() {
        let mut s: SnapshotableSlab<i32> = SnapshotableSlab::default();
        // Should not panic even if key doesn't exist
        s.modify(999, |v| *v += 1);
        assert!(
            s.get(999).is_none(),
            "modify on missing key should be a no-op; get(999) must be None"
        );
    }
}
