#![cfg(feature = "cache")]
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use steve_runtime::{FakeClock, cache::*};
static SPACE: CacheSpace<String, usize> = CacheSpace {
    name: "contract",
    ttl: Duration::from_secs(5),
    share_pct: 50,
    tags: |k| vec![Tag::of("entity", k)],
    weigh: |_| 64,
};
fn cfg(mode: CacheMode) -> CacheConfig {
    CacheConfig { mode, memory_mb: 1 }
}

#[tokio::test]
async fn concurrent_hundred_reads_share_one_load() {
    let caches = Caches::new(cfg(CacheMode::Memory));
    let cache = caches.space(&SPACE);
    let barrier = Arc::new(tokio::sync::Barrier::new(101));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..100 {
        let (c, b, e, r, n) = (
            cache.clone(),
            barrier.clone(),
            entered.clone(),
            release.clone(),
            calls.clone(),
        );
        tasks.push(tokio::spawn(async move {
            b.wait().await;
            c.get_or_load("a".into(), || async {
                n.fetch_add(1, Ordering::SeqCst);
                e.notify_one();
                r.notified().await;
                Ok::<_, ()>(7)
            })
            .await
            .unwrap()
        }));
    }
    barrier.wait().await;
    entered.notified().await;
    release.notify_one();
    for task in tasks {
        assert_eq!(*task.await.unwrap(), 7);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn invalidation_during_a_held_load_cannot_poison_a_new_read() {
    let caches = Caches::new(cfg(CacheMode::Memory));
    let c = caches.space(&SPACE);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (old, e, r) = (c.clone(), entered.clone(), release.clone());
    let old = tokio::spawn(async move {
        old.get_or_load("a".into(), || async {
            e.notify_one();
            r.notified().await;
            Ok::<_, ()>(1)
        })
        .await
        .unwrap()
    });
    entered.notified().await;
    caches.invalidate(&[Tag::of("entity", "a")]);
    let new = c.clone();
    let new = tokio::spawn(async move {
        new.get_or_load("a".into(), || async { Ok::<_, ()>(2) })
            .await
            .unwrap()
    });
    release.notify_one();
    assert_eq!(*old.await.unwrap(), 1);
    assert_eq!(*new.await.unwrap(), 2);
    assert_eq!(*c.peek(&"a".into()).unwrap(), 2);
    assert_eq!(
        c.len(),
        1,
        "generation is in the value, not a new cache key"
    );
}
#[tokio::test]
async fn ttl_boundary_uses_the_injected_clock_and_writes_are_immediately_visible() {
    let clock = Arc::new(FakeClock::new(0));
    let caches = Caches::with_clock(cfg(CacheMode::Memory), clock.clone());
    let c = caches.space(&SPACE);
    assert_eq!(
        *c.get_or_load("a".into(), || async { Ok::<_, ()>(1) })
            .await
            .unwrap(),
        1
    );
    clock.advance(4999);
    assert_eq!(
        *c.get_or_load("a".into(), || async { Ok::<_, ()>(2) })
            .await
            .unwrap(),
        1
    );
    clock.advance(1);
    assert!(c.peek(&"a".into()).is_none());
    assert_eq!(
        *c.get_or_load("a".into(), || async { Ok::<_, ()>(2) })
            .await
            .unwrap(),
        2
    );
    caches.invalidate(&[Tag::of("entity", "a")]);
    assert_eq!(
        *c.get_or_load("a".into(), || async { Ok::<_, ()>(3) })
            .await
            .unwrap(),
        3
    );
}
#[tokio::test]
async fn off_bypasses_and_errors_never_become_values() {
    for mode in [CacheMode::Memory, CacheMode::Off] {
        let c = Caches::new(cfg(mode)).space(&SPACE);
        assert_eq!(
            c.get_or_load("a".into(), || async { Err::<usize, _>("failure") })
                .await,
            Err("failure")
        );
        assert_eq!(
            *c.get_or_load("a".into(), || async { Ok::<_, &str>(1) })
                .await
                .unwrap(),
            1
        );
        let v = c
            .get_or_load("a".into(), || async { Ok::<_, &str>(2) })
            .await
            .unwrap();
        assert_eq!(*v, if mode == CacheMode::Off { 2 } else { 1 });
        if mode == CacheMode::Off {
            assert!(c.peek(&"a".into()).is_none());
            assert_eq!(c.usage().await, (0, 0));
        }
    }
}
static SMALL: CacheSpace<String, usize> = CacheSpace {
    name: "small",
    ttl: Duration::from_secs(5),
    share_pct: 25,
    tags: |_| vec![],
    weigh: |n| *n as u32,
};
static LARGE: CacheSpace<String, usize> = CacheSpace {
    name: "large",
    ttl: Duration::from_secs(5),
    share_pct: 75,
    tags: |_| vec![],
    weigh: |n| *n as u32,
};
#[tokio::test]
async fn registered_spaces_share_the_byte_budget_and_keep_one_instance() {
    let caches = Caches::new(cfg(CacheMode::Memory));
    let small = caches.space(&SMALL);
    let large = caches.space(&LARGE);
    assert_eq!(small.capacity(), 262144);
    assert_eq!(large.capacity(), 786432);
    for i in 0..20 {
        small
            .get_or_load(i.to_string(), || async { Ok::<_, ()>(100_000) })
            .await
            .unwrap();
    }
    assert!(small.usage().await.1 <= small.capacity());
    let same = caches.space(&SMALL);
    assert_eq!(same.usage().await, small.usage().await);
}
#[test]
fn bounded_tag_table_preserves_global_monotonicity_and_long_utf8_ids_are_hashed() {
    let caches = Caches::new(cfg(CacheMode::Memory));
    let a = Tag::of("state", "a");
    let b = Tag::of("state", "b");
    caches.invalidate(&[a.clone()]);
    assert_eq!(caches.generation(&a), 1);
    assert_eq!(caches.generation(&b), 0);
    caches.invalidate(&[Tag::global("state")]);
    assert_eq!(caches.generation(&b), 1);
    for i in 0..10_100 {
        caches.invalidate(&[Tag::of("state", &i.to_string())]);
    }
    assert!(caches.generation(&a) >= 2 && caches.generation(&b) >= 1);
    let before = caches.generation(&b);
    caches.invalidate(&[a.clone()]);
    assert_eq!(caches.generation(&b), before);
    let long = "标签".repeat(100);
    let tag = Tag::of("id", &long);
    assert_eq!(tag, Tag::of("id", &long));
    assert_ne!(tag, Tag::of("id", &(long + "x")));
    assert!(format!("{tag:?}").len() < 180);
}
#[tokio::test]
async fn a_stale_conditional_network_response_is_not_republished_after_invalidation() {
    let caches = Caches::new(cfg(CacheMode::Memory));
    let cache = caches.space(&SPACE);
    let key = "a".to_string();
    let version = cache.version(&key);
    caches.invalidate(&[Tag::of("entity", "a")]);
    assert!(!cache.put_if_current(key.clone(), 1, version).await);
    assert!(cache.peek(&key).is_none());
    assert!(
        cache
            .put_if_current(key.clone(), 2, cache.version(&key))
            .await
    );
    assert_eq!(*cache.peek(&key).unwrap(), 2);
    let version = cache.version(&key);
    cache.clear();
    assert!(!cache.put_if_current(key.clone(), 3, version).await);
    assert!(cache.peek(&key).is_none());
}
