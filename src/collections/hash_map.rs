use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::ops::{Index, IndexMut};

/// A thin wrapper around `std::collections::HashMap` that can take a point-in-time snapshot.
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
pub struct SnapshotableHashMap<K, V> {
    map: HashMap<K, V>,
    snap: Option<SnapshotCtx<K, V>>,
}

#[derive(Debug)]
struct SnapshotCtx<K, V> {
    /// Keys inserted after the snapshot was started.
    keys_after: HashSet<K>,
    /// Old values for keys that have been changed/removed since the snapshot began.
    old: HashMap<K, V>,
}

impl<K: Eq + Hash + Clone, V> Default for SnapshotableHashMap<K, V> {
    fn default() -> Self {
        Self {
            map: HashMap::new(),
            snap: None,
        }
    }
}

impl<K, V> SnapshotableHashMap<K, V>
where
    K: Eq + Hash + Clone + Ord,
    V: Clone,
{
    /// Begins a snapshot and returns a vector of the key list that existed at that moment.
    ///
    /// Panics if a snapshot is already active.
    pub fn begin_snapshot(&mut self) -> Vec<K> {
        assert!(self.snap.is_none());
        let ctx = SnapshotCtx {
            keys_after: HashSet::new(),
            old: HashMap::new(),
        };
        self.snap = Some(ctx);
        self.map.keys().cloned().collect()
    }

    /// Ends the current snapshot (if any), returning to normal semantics.
    pub fn end_snapshot(&mut self) {
        self.snap = None;
    }

    /// If a snapshot is active and `key` was present at snapshot start, stash old value once.
    fn maybe_stash_old(&mut self, key: &K) {
        if let Some(ctx) = &mut self.snap {
            if ctx.old.contains_key(key) {
                return;
            }
            if !ctx.keys_after.contains(key)
                && let Some(old) = self.map.get(key).cloned()
            {
                ctx.old.insert(key.clone(), old);
            }
        }
    }

    /// Inserts a value into the underlying map. Returns the previous value, if any.
    pub fn insert(&mut self, k: K, v: V) -> Option<V> {
        if let Some(snap) = &mut self.snap {
            snap.keys_after.insert(k.clone());
        }
        self.map.insert(k, v)
    }

    /// Gets the current value for `key`, regardless of snapshot state.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.map.get(key)
    }

    /// Gets a value suitable for readers during a snapshot.
    ///
    /// - If a snapshot is active and an old value was stashed for `key`, that old value is returned.
    /// - Otherwise, returns the current value from the map.
    pub fn get_for_snapshot(&self, key: &K) -> Option<&V> {
        match &self.snap {
            Some(ctx) => {
                if let Some(old) = ctx.old.get(key) {
                    return Some(old);
                }
                self.map.get(key)
            }
            None => self.map.get(key),
        }
    }

    /// Mutates an entry by key using the provided closure. No-op if the key doesn't exist.
    pub fn modify<F: FnOnce(&mut V)>(&mut self, key: &K, f: F) {
        self.maybe_stash_old(key);
        if let Some(v) = self.map.get_mut(key) {
            f(v);
        }
    }

    /// Removes and returns the current value for `key`, if any.
    /// If a snapshot is active and `key` was present at snapshot start, the old value is stashed first.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        self.maybe_stash_old(key);
        if let Some(snap) = &mut self.snap {
            snap.keys_after.remove(key);
        }
        self.map.remove(key)
    }
}

impl<K, V> Index<&K> for SnapshotableHashMap<K, V>
where
    K: Eq + Hash,
{
    type Output = V;

    fn index(&self, key: &K) -> &Self::Output {
        &self.map[key]
    }
}

impl<K, V> IndexMut<&K> for SnapshotableHashMap<K, V>
where
    K: Eq + Hash + Clone + Ord,
    V: Clone,
{
    fn index_mut(&mut self, key: &K) -> &mut Self::Output {
        self.maybe_stash_old(key);
        self.map.get_mut(key).expect("key not found")
    }
}

#[cfg(test)]
mod tests {
    use super::SnapshotableHashMap;

    #[test]
    fn begin_and_end_snapshot() {
        let mut m: SnapshotableHashMap<u32, i32> = SnapshotableHashMap::default();
        m.insert(1, 10);
        m.insert(2, 20);

        let mut keys = m.begin_snapshot();
        keys.sort_unstable();
        assert_eq!(keys, vec![1, 2]);
        assert_eq!(m.get_for_snapshot(&1), Some(&10));
        assert_eq!(m.get_for_snapshot(&2), Some(&20));

        m.end_snapshot();
        assert_eq!(m.get_for_snapshot(&1), Some(&10));
    }

    #[test]
    fn get_for_snapshot_returns_old_on_modify_and_remove() {
        let mut m: SnapshotableHashMap<&'static str, i32> = SnapshotableHashMap::default();
        m.insert("a", 1);
        m.insert("b", 2);
        m.insert("c", 3);

        let mut keys = m.begin_snapshot();
        keys.sort_unstable();
        assert_eq!(keys, vec!["a", "b", "c"]);

        // Modify existing key
        m.modify(&"b", |v| *v = 200);
        assert_eq!(m.get(&"b"), Some(&200));
        assert_eq!(m.get_for_snapshot(&"b"), Some(&2)); // old value

        // Remove existing key
        let removed = m.remove(&"c");
        assert_eq!(removed, Some(3));
        assert_eq!(m.get(&"c"), None);
        assert_eq!(m.get_for_snapshot(&"c"), Some(&3)); // old value

        // Key not touched returns current
        assert_eq!(m.get_for_snapshot(&"a"), Some(&1));
    }

    #[test]
    fn new_insertions_are_not_stashed() {
        let mut m: SnapshotableHashMap<i32, i32> = SnapshotableHashMap::default();
        m.insert(10, 100);

        let _ = m.begin_snapshot();

        // Insertion after snapshot start: not part of snapshot, so no stashing
        m.insert(11, 110);
        m.modify(&11, |v| *v = 111);
        assert_eq!(m.get_for_snapshot(&11), Some(&111));

        // Removing also shows None to snapshot readers (not stashed)
        m.remove(&11);
        assert_eq!(m.get_for_snapshot(&11), None);
    }

    #[test]
    fn index_mut_triggers_stash() {
        let mut m: SnapshotableHashMap<i32, i32> = SnapshotableHashMap::default();
        m.insert(1, 10);
        m.insert(2, 20);
        m.begin_snapshot();

        m[&1] = 100; // should stash old value (10)
        assert_eq!(m.get_for_snapshot(&1), Some(&10));

        m[&2] += 1; // should stash old value (20)
        assert_eq!(m.get_for_snapshot(&2), Some(&20));
    }

    #[test]
    fn modify_noop_for_missing_key() {
        let mut m: SnapshotableHashMap<i32, i32> = SnapshotableHashMap::default();
        m.insert(1, 10);
        m.begin_snapshot();

        m.modify(&999, |v| *v = *v + 1); // no-op, key missing
        assert_eq!(m.get_for_snapshot(&1), Some(&10));
    }
}
