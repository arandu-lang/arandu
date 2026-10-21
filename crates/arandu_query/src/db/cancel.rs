#[derive(Clone, Debug)]
pub struct QueryCancellationToken(pub(super) salsa::CancellationToken);

impl QueryCancellationToken {
    pub fn cancel(&self) {
        self.0.cancel();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueryCancelled;

pub fn catch_query_cancellation<T>(operation: impl FnOnce() -> T) -> Result<T, QueryCancelled> {
    salsa::Cancelled::catch(std::panic::AssertUnwindSafe(operation)).map_err(|_| QueryCancelled)
}
