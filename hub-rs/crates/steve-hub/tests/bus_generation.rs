//! Cache generations advance synchronously with publication.
mod bus_cases;

use bus_cases::typed;
use steve_hub::bus::Bus;
use steve_runtime::cache::Tag;

#[test]
fn only_stats_publish_stats_cache_generations() {
    let bus = Bus::new(std::sync::Arc::new(steve_runtime::cache::Caches::new(
        Default::default(),
    )));
    let a = Tag::of("stats", "a");
    let b = Tag::of("stats", "b");
    let all = Tag::global("stats-events");
    let generation = |tag| bus.caches().generation(tag);
    for ty in &steve_types::WIRE_TYPES {
        if *ty != "stats" {
            bus.publish(typed(ty, "a"));
        }
    }
    assert_eq!(generation(&all), 0);
    bus.publish(typed("stats", "a"));
    assert_eq!(
        (generation(&a), generation(&b), generation(&all)),
        (1, 0, 1)
    );
    bus.publish(typed("stats", "b"));
    assert_eq!(
        (generation(&a), generation(&b), generation(&all)),
        (1, 1, 2)
    );
}
