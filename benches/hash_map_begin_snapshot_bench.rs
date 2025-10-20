use chortke::collections::hash_map::SnapshotableHashMap;
use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn bench_hash_map_begin_snapshot_u64_million(c: &mut Criterion) {
    // Prepare a map with 5,000,000 u64 keys
    let mut map: SnapshotableHashMap<u64, u64> = SnapshotableHashMap::default();
    for i in 0u64..5_000_000 {
        map.insert(i, i);
    }

    c.bench_function("SnapshotableHashMap::begin_snapshot on 5m u64 keys", |b| {
        b.iter(|| {
            // Time only the snapshot begin and end operations
            let keys = map.begin_snapshot();
            black_box(&keys);
            // Make sure to end the snapshot so the next iteration can start a new one
            map.end_snapshot();
        });
    });
}

criterion_group!(benches, bench_hash_map_begin_snapshot_u64_million);
criterion_main!(benches);
