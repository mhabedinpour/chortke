use chortke::collections::slab::SnapshotableSlab;
use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn bench_slab_begin_snapshot_million(c: &mut Criterion) {
    // Prepare a slab with 5,000,000 entries
    let mut slab: SnapshotableSlab<u64> = SnapshotableSlab::default();
    for i in 0u64..5_000_000 {
        let _ = slab.insert(i);
    }

    c.bench_function("SnapshotableSlab::begin_snapshot on 5m entries", |b| {
        b.iter(|| {
            let keys_arc = slab.begin_snapshot();
            black_box(&keys_arc);
            slab.end_snapshot();
        });
    });
}

criterion_group!(benches, bench_slab_begin_snapshot_million);
criterion_main!(benches);
