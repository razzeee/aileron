use std::future::Future;
use std::sync::OnceLock;

type LocalJob = Box<dyn FnOnce() + Send>;

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("aileron-manager-ipc")
            .build()
            .expect("failed to create manager IPC runtime")
    })
}

/// Run IPC work on Tokio, then return its owned result to the GLib main context.
pub fn spawn<F, T, C>(future: F, callback: C)
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
    C: FnOnce(T) + 'static,
{
    let task = runtime().spawn(future);
    glib::spawn_future_local(async move {
        match task.await {
            Ok(output) => callback(output),
            Err(error) => tracing::error!(%error, "manager IPC task failed"),
        }
    });
}

fn local_sender() -> &'static tokio::sync::mpsc::UnboundedSender<LocalJob> {
    static SENDER: OnceLock<tokio::sync::mpsc::UnboundedSender<LocalJob>> = OnceLock::new();
    SENDER.get_or_init(|| {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<LocalJob>();
        std::thread::Builder::new()
            .name("aileron-manager-ipc-streams".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to create manager streaming runtime");
                let local = tokio::task::LocalSet::new();
                local.block_on(&runtime, async move {
                    while let Some(job) = receiver.recv().await {
                        job();
                    }
                });
            })
            .expect("failed to start manager streaming runtime");
        sender
    })
}

/// Construct and run a `!Send` zlink stream on a dedicated Tokio `LocalSet`.
pub fn spawn_local<FN, F, T, C>(factory: FN, callback: C)
where
    FN: FnOnce() -> F + Send + 'static,
    F: Future<Output = T> + 'static,
    T: Send + 'static,
    C: FnOnce(T) + 'static,
{
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let job = Box::new(move || {
        tokio::task::spawn_local(async move {
            let _ = sender.send(factory().await);
        });
    });
    if local_sender().send(job).is_err() {
        tracing::error!("manager streaming runtime stopped");
        return;
    }
    glib::spawn_future_local(async move {
        if let Ok(output) = receiver.await {
            callback(output);
        }
    });
}
