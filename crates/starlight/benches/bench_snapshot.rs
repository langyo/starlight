use criterion::{criterion_group, criterion_main, Criterion};
use starlight::{
    heap::snapshot::{deserializer::Deserializer, Snapshot},
    vm::{GcParams, Runtime},
    Platform,
};
criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);

fn criterion_benchmark(c: &mut Criterion) {
    Platform::initialize();
    let mut initial_rt = Runtime::new(GcParams::default(), None);
    let snapshot = Snapshot::take(false, &mut initial_rt).buffer.to_vec();

    c.bench_function("runtime from scratch", |b| {
        b.iter_with_large_drop(|| Runtime::new(GcParams::default(), None));
    });

    c.bench_function("runtime from snapshot", |b| {
        b.iter_with_large_drop(|| {
            Deserializer::deserialize(false, &snapshot, GcParams::default(), None)
        });
    });
}
