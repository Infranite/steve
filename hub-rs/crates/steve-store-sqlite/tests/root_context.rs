//! Public Tokio context observations before turning the injected grant probe into
//! a rejection. Runtime/task presence and remaining budget are not worker identity.
#[derive(Debug, PartialEq, Eq)]
struct Context {
    runtime: bool,
    task: bool,
    budget: bool,
}
fn observe() -> Context {
    Context {
        runtime: tokio::runtime::Handle::try_current().is_ok(),
        task: tokio::task::try_id().is_some(),
        budget: tokio::task::coop::has_budget_remaining(),
    }
}
#[test]
fn public_context_flags_do_not_identify_blocking_workers() {
    assert_eq!(
        observe(),
        Context {
            runtime: false,
            task: false,
            budget: true
        }
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();
    runtime.block_on(async {
        let worker = tokio::spawn(async { observe() }).await.unwrap();
        let blocking = tokio::task::spawn_blocking(observe).await.unwrap();
        let in_place = tokio::spawn(async { tokio::task::block_in_place(observe) })
            .await
            .unwrap();
        println!("worker={worker:?}; spawn_blocking={blocking:?}; block_in_place={in_place:?}");
        assert_eq!(
            worker,
            Context {
                runtime: true,
                task: true,
                budget: true
            }
        );
        assert_eq!(
            blocking, worker,
            "Handle/task/budget presence cannot reject only workers"
        );
        assert_eq!(
            in_place, worker,
            "block_in_place releases scheduler ownership without clearing these flags"
        );
    });
    let current = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    current.block_on(async {
        let context = tokio::spawn(async { observe() }).await.unwrap();
        println!("current_thread={context:?}");
        assert_eq!(
            context,
            Context {
                runtime: true,
                task: true,
                budget: true
            }
        );
    });
}
