use std::marker::PhantomData;

use async_task::Runnable;
use flume::{Receiver, Sender};
use futures_lite::Future;
use thread_priority::ThreadBuilder;

pub use async_task::Task;
pub use thread_priority::ThreadPriority;

struct ForegroundThreadExecutor {
    rx: Receiver<Runnable>,
    // A ZST that makes this type !Send and !Sync
    marker: PhantomData<*const ()>,
}

impl ForegroundThreadExecutor {
    fn new(rx: Receiver<Runnable>) -> Self {
        Self {
            rx,
            marker: PhantomData::default(),
        }
    }

    fn thread_loop(&self) {
        for runnable in &self.rx {
            runnable.run();
        }
    }
}

struct BackgroundThreadExecutor {
    rx: Receiver<Runnable>,
}

impl BackgroundThreadExecutor {
    fn new(rx: Receiver<Runnable>) -> Self {
        Self { rx }
    }

    fn thread_loop(&self) {
        for runnable in &self.rx {
            runnable.run();
        }
    }
}

#[derive(Clone)]
struct ForegroundTaskDispatcher {
    tx: Sender<Runnable>,
}

impl ForegroundTaskDispatcher {
    pub fn spawn<R>(&self, future: impl Future<Output = R> + 'static) -> Task<R>
    where
        R: 'static,
    {
        let thread_handle = self.tx.clone();
        let schedule = move |runnable| thread_handle.send(runnable).unwrap();
        let (runnable, task) = async_task::spawn_local(future, schedule);
        runnable.schedule();
        task
    }
}

#[derive(Clone)]
struct BackgroundTaskDispatcher;

impl BackgroundTaskDispatcher {
    fn new() -> Self {
        Self
    }

    pub fn spawn<R>(
        &self,
        priority: ThreadPriority,
        future: impl Future<Output = R> + Send + 'static,
    ) -> Task<R>
    where
        R: Send + 'static,
    {
        let (tx, rx) = flume::unbounded();
        let _thread = ThreadBuilder::default()
            .priority(priority)
            .spawn_careless(move || {
                let executor = BackgroundThreadExecutor::new(rx);
                executor.thread_loop()
            });
        let schedule = move |runnable| tx.send(runnable).unwrap();
        let (runnable, task) = async_task::spawn(future, schedule);
        runnable.schedule();
        task
    }
}

/// Contextual access to the [`PriorityExecutor`] from within the foreground thread
#[derive(Clone)]
pub struct PriorityExecutorContext {
    backgroud_task_dispatcher: BackgroundTaskDispatcher,
    foreground_task_dispatcher: ForegroundTaskDispatcher,
}

impl PriorityExecutorContext {
    /// Spawn a task in the foreground on the current thread. Use this for critical tasks.
    pub fn spawn<Fut, R>(&self, f: impl FnOnce(Self) -> Fut) -> Task<R>
    where
        Fut: Future<Output = R> + 'static,
        R: 'static,
    {
        self.foreground_task_dispatcher.spawn(f(self.clone()))
    }

    /// Spawn a task in a new thread with a given thread priority. Use this method with a low
    /// priority for non-critical tasks that shouldn't block the foreground thread.
    ///
    /// Running a background task with a higher priority than the current thread will likely
    /// result in the foreground thread becoming blocked by the OS scheduler.
    pub fn spawn_background<R>(
        &self,
        priority: ThreadPriority,
        future: impl Future<Output = R> + Send + 'static,
    ) -> Task<R>
    where
        R: Send + 'static,
    {
        self.backgroud_task_dispatcher.spawn(priority, future)
    }
}

/// A dual executor for handling both critical and non-critical async tasks
///
/// You probably only want one of these for the lifecycle of your software, like so:
/// ```no_run
/// async fn async_main(cx: PriorityExecutorContext) {
///    //...
/// }
///
/// fn main() {
///     let executor = PriorityExecutor::new();
///     executor.spawn(async_main).detach();
///     executor.run();
/// }
/// ```
pub struct PriorityExecutor {
    background_task_dispatcher: BackgroundTaskDispatcher,
    foreground_task_dispatcher: ForegroundTaskDispatcher,
    foreground_thread_executor: ForegroundThreadExecutor,
}

impl PriorityExecutor {
    pub fn new() -> Self {
        let background_task_dispatcher = BackgroundTaskDispatcher::new();
        let (tx, rx) = flume::unbounded();
        let foreground_thread_executor = ForegroundThreadExecutor::new(rx);
        let foreground_task_dispatcher = ForegroundTaskDispatcher { tx };
        Self {
            background_task_dispatcher,
            foreground_task_dispatcher,
            foreground_thread_executor,
        }
    }

    fn to_async(&self) -> PriorityExecutorContext {
        PriorityExecutorContext {
            backgroud_task_dispatcher: self.background_task_dispatcher.clone(),
            foreground_task_dispatcher: self.foreground_task_dispatcher.clone(),
        }
    }

    /// Spawn a task in the foreground on the current thread. Use this for critical tasks.
    pub fn spawn<Fut, R>(&self, f: impl FnOnce(PriorityExecutorContext) -> Fut) -> Task<R>
    where
        Fut: Future<Output = R> + 'static,
        R: 'static,
    {
        self.foreground_task_dispatcher.spawn(f(self.to_async()))
    }

    /// Spawn a task in a new thread with a given thread priority. Use this method with a low
    /// priority for non-critical tasks that shouldn't block the foreground thread.
    ///
    /// Running a background task with a higher priority than the current thread will likely
    /// result in the foreground thread becoming blocked by the OS scheduler.
    pub fn spawn_background<R>(
        &self,
        priority: ThreadPriority,
        future: impl Future<Output = R> + Send + 'static,
    ) -> Task<R>
    where
        R: Send + 'static,
    {
        self.background_task_dispatcher.spawn(priority, future)
    }

    /// Run the executor
    pub fn run(&self) {
        self.foreground_thread_executor.thread_loop()
    }
}
