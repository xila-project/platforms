pub use embassy_executor::Executor;

/// Share the static storage of the executor with the other drivers.
#[doc(hidden)]
pub use drivers_shared as __shared;

#[macro_export]
macro_rules! instantiate_static_executor {
    () => {
        $crate::executor::__shared::instantiate_static_executor!($crate::executor::Executor)
    };
}

pub use instantiate_static_executor;
