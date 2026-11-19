use super::*;
use std::sync::atomic::AtomicUsize;
static SPACE: CacheSpace<String, usize> = CacheSpace {
    name: "contract",
    ttl: Duration::from_secs(5),
    stale_for: Duration::ZERO,
    share_pct: 50,
    tags: |k| vec![Tag::of("entity", k)],
    weigh: |_| 64,
};
fn caches(mode: CacheMode) -> Caches {
    Caches::new(CacheConfig { mode, memory_mb: 1 })
}
#[tokio::test]
async fn concurrent_hundred_reads_share_one_load() {
    let caches = caches(CacheMode::Memory);
    let cache = caches.space(&SPACE);
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut tasks = Vec::new();
    for _ in 0..100 {
        let (cache, calls, entered, release) = (
            cache.clone(),
            calls.clone(),
            entered.clone(),
            release.clone(),
        );
        tasks.push(tokio::spawn(async move {
            cache
                .get_or_load("a".into(), || async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    entered.notify_one();
                    release.notified().await;
                    Ok::<_, ()>(7)
                })
                .await
                .unwrap()
        }));
    }
    entered.notified().await;
    release.notify_one();
    for task in tasks {
        assert_eq!(*task.await.unwrap(), 7);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn invalidation_during_a_held_load_cannot_poison_a_new_read() {
    let caches = caches(CacheMode::Memory);
    let cache = caches.space(&SPACE);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (old, e, r) = (cache.clone(), entered.clone(), release.clone());
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
    let new = cache.clone();
    let new = tokio::spawn(async move {
        new.get_or_load("a".into(), || async { Ok::<_, ()>(2) })
            .await
            .unwrap()
    });
    release.notify_one();
    assert_eq!(*old.await.unwrap(), 1);
    assert_eq!(*new.await.unwrap(), 2);
    assert_eq!(*cache.peek(&"a".into()).await.unwrap().0, 2);
}
#[tokio::test(start_paused = true)]
async fn ttl_uses_the_tokio_clock_and_invalidated_stale_values_are_hidden() {
    let caches = caches(CacheMode::Memory);
    let cache = caches.space(&SPACE);
    cache
        .get_or_load("a".into(), || async { Ok::<_, ()>(1) })
        .await
        .unwrap();
    tokio::time::advance(Duration::from_millis(4999)).await;
    assert_eq!(
        *cache
            .get_or_load("a".into(), || async { Ok::<_, ()>(2) })
            .await
            .unwrap(),
        1
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(cache.peek(&"a".into()).await.is_none());
    assert_eq!(
        *cache
            .get_or_load("a".into(), || async { Ok::<_, ()>(2) })
            .await
            .unwrap(),
        2
    );
    caches.invalidate(&[Tag::of("entity", "a")]);
    assert!(cache.peek(&"a".into()).await.is_none());
    assert_eq!(
        *cache
            .get_or_load("a".into(), || async { Ok::<_, ()>(3) })
            .await
            .unwrap(),
        3
    );
}
#[tokio::test]
async fn off_has_no_moka_and_errors_are_not_cached() {
    for mode in [CacheMode::Memory, CacheMode::Off] {
        let cache = caches(mode).space(&SPACE);
        assert_eq!(cache.cache.is_none(), mode == CacheMode::Off);
        assert!(
            cache
                .get_or_load("a".into(), || async { Err::<usize, _>("failed") })
                .await
                .is_err()
        );
        assert_eq!(
            *cache
                .get_or_load("a".into(), || async { Ok::<_, ()>(1) })
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            *cache
                .get_or_load("a".into(), || async { Ok::<_, ()>(2) })
                .await
                .unwrap(),
            if mode == CacheMode::Off { 2 } else { 1 }
        );
    }
}
static SMALL: CacheSpace<String, usize> = CacheSpace {
    name: "small",
    ttl: Duration::from_secs(5),
    stale_for: Duration::ZERO,
    share_pct: 25,
    tags: |_| vec![],
    weigh: |n| *n as u32,
};
#[tokio::test]
async fn byte_budget_and_administrative_registry_do_not_erase_types() {
    let caches = caches(CacheMode::Memory);
    let small = caches.space(&SMALL);
    assert_eq!(
        small.cache.as_ref().unwrap().policy().max_capacity(),
        Some(262144)
    );
    for i in 0..20 {
        small
            .get_or_load(i.to_string(), || async { Ok::<_, ()>(100_000) })
            .await
            .unwrap();
    }
    small.cache.as_ref().unwrap().run_pending_tasks().await;
    let admin = caches.0.spaces.read().unwrap();
    assert_eq!(admin[0].name(), "small");
    assert_eq!(admin[0].share(), 25);
    assert!(admin[0].usage().1 <= 262144);
}
#[tokio::test]
async fn table_rollover_and_conditional_responses_cannot_revalidate_revoked_values() {
    let caches = caches(CacheMode::Memory);
    let cache = caches.space(&SPACE);
    let key = "a".to_owned();
    let old = cache.version(&key);
    for i in 0..10_100 {
        caches.invalidate(&[Tag::of("entity", &i.to_string())]);
    }
    assert!(!cache.put_if_current(key.clone(), 1, old).await);
    let current = cache.version(&key);
    assert!(cache.put_if_current(key.clone(), 2, current).await);
    assert_eq!(*cache.peek(&key).await.unwrap().0, 2);
    caches.invalidate(&[Tag::global("entity")]);
    assert!(cache.peek(&key).await.is_none());
    let long = "标签".repeat(100);
    assert_eq!(Tag::of("id", &long), Tag::of("id", &long));
    assert_ne!(Tag::of("id", &long), Tag::of("id", &(long + "x")));
}
static TAG_CALLS: AtomicUsize = AtomicUsize::new(0);
static HOT: CacheSpace<String, usize> = CacheSpace {
    name: "hot",
    ttl: Duration::from_secs(5),
    stale_for: Duration::ZERO,
    share_pct: 50,
    tags: |k| {
        TAG_CALLS.fetch_add(1, Ordering::SeqCst);
        vec![Tag::of("hot", k)]
    },
    weigh: |_| 1,
};
#[tokio::test]
async fn hits_do_not_rebuild_tags_and_constant_invalidation_finishes_the_request() {
    let caches = caches(CacheMode::Memory);
    let cache = caches.space(&HOT);
    let key = "a".to_owned();
    cache
        .get_or_load(key.clone(), || async { Ok::<_, ()>(1) })
        .await
        .unwrap();
    let before = TAG_CALLS.load(Ordering::SeqCst);
    for _ in 0..100 {
        cache
            .get_or_load(key.clone(), || async { Ok::<_, ()>(9) })
            .await
            .unwrap();
    }
    assert_eq!(TAG_CALLS.load(Ordering::SeqCst), before);
    caches.invalidate(&[Tag::of("hot", &key)]);
    let value = cache
        .get_or_load(key.clone(), || async {
            caches.invalidate(&[Tag::of("hot", &key)]);
            Ok::<_, ()>(2)
        })
        .await
        .unwrap();
    assert_eq!(*value, 2);
    assert!(cache.peek(&key).await.is_none());
}

#[test]
#[should_panic(expected = "registered twice during assembly")]
fn duplicate_space_is_rejected_before_serving_requests() {
    let registry = caches(CacheMode::Memory);
    let _first = registry.space(&SPACE);
    let _second = registry.space(&SPACE);
}
static GRACE: CacheSpace<String, usize> = CacheSpace {
    name: "grace",
    ttl: Duration::from_millis(20),
    stale_for: Duration::from_secs(60),
    share_pct: 50,
    tags: |key| vec![Tag::of("grace", key)],
    weigh: |_| 1,
};
#[tokio::test(start_paused = true)]
async fn stale_grace_keeps_one_readable_value_while_refresh_is_held() {
    let registry = caches(CacheMode::Memory);
    let cache = registry.space(&GRACE);
    assert_eq!(
        cache.cache.as_ref().unwrap().policy().time_to_live(),
        Some(GRACE.ttl + GRACE.stale_for)
    );
    cache
        .get_or_load("a".into(), || async { Ok::<_, ()>(1) })
        .await
        .unwrap();
    tokio::time::advance(GRACE.ttl).await;
    let (old, fresh) = cache.peek(&"a".into()).await.unwrap();
    assert_eq!(*old, 1);
    assert!(!fresh);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (c, e, r) = (cache.clone(), entered.clone(), release.clone());
    let refresh = tokio::spawn(async move {
        c.get_or_load("a".into(), || async {
            e.notify_one();
            r.notified().await;
            Ok::<_, ()>(2)
        })
        .await
        .unwrap()
    });
    entered.notified().await;
    assert_eq!(
        *cache.peek(&"a".into()).await.unwrap().0,
        1,
        "refresh must not remove the only display value"
    );
    release.notify_one();
    assert_eq!(*refresh.await.unwrap(), 2);
    tokio::time::advance(GRACE.ttl + GRACE.stale_for).await;
    assert!(cache.peek(&"a".into()).await.is_none());
}
// The card explicitly permits a minimal real wait to catch the two-clock regression.
// Logical-time tests above additionally cover the exact grace-end boundary without waiting.
#[tokio::test]
async fn production_clock_retains_a_value_after_its_fresh_ttl() {
    let cache = caches(CacheMode::Memory).space(&GRACE);
    cache
        .get_or_load("a".into(), || async { Ok::<_, ()>(1) })
        .await
        .unwrap();
    tokio::time::sleep(GRACE.ttl + Duration::from_millis(5)).await;
    assert_eq!(*cache.peek(&"a".into()).await.unwrap().0, 1);
    assert!(!cache.peek(&"a".into()).await.unwrap().1);
}
#[tokio::test]
async fn a_post_write_reader_uses_its_own_uncached_load_at_the_retry_limit() {
    let registry = caches(CacheMode::Memory);
    let cache = registry.space(&SPACE);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (c, e, r) = (cache.clone(), entered.clone(), release.clone());
    let first = tokio::spawn(async move {
        c.get_or_load("a".into(), || async {
            e.notify_one();
            r.notified().await;
            Ok::<_, ()>(1)
        })
        .await
        .unwrap()
    });
    entered.notified().await;
    registry.invalidate(&[Tag::of("entity", "a")]);
    let called = AtomicUsize::new(0);
    // One remaining attempt exercises the terminal branch without relying on the
    // timing of three initializer-pool turnovers between these two readers.
    let second = cache.load_miss(
        "a".into(),
        || async {
            called.fetch_add(1, Ordering::SeqCst);
            Ok::<_, ()>(9)
        },
        1,
    );
    futures::pin_mut!(second);
    assert!(futures::poll!(second.as_mut()).is_pending());
    registry.invalidate(&[Tag::of("entity", "a")]);
    registry.invalidate(&[Tag::of("entity", "a")]);
    release.notify_one();
    assert_eq!(*first.await.unwrap(), 1);
    assert_eq!(*second.await.unwrap(), 9);
    assert_eq!(called.load(Ordering::SeqCst), 1);
    assert!(
        cache.peek(&"a".into()).await.is_none(),
        "terminal fallback must not publish"
    );
}
