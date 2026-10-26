//! Per-block hooks of the long whole-archive walks (`verify`, `check_recovery`, the repair):
//! after each block (a data block for `verify`, a recovery frame for the recovery walks) the
//! walk calls the hook with its position, and the hook answers whether to go on. A front end
//! reports progress and polls its cancel token there (E5-1); the plain functions pass
//! [`no_hook`], so their behaviour is unchanged.

use std::ops::ControlFlow;

/// Where a walk stands: `done` of `total` units (blocks, or recovery frames) finished.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockStep {
    /// Units finished.
    pub done: u64,
    /// Units in the walk.
    pub total: u64,
}

/// The hook: `Break` stops the walk, which then returns `Ok(None)`.
pub type BlockHook<'a> = dyn FnMut(BlockStep) -> ControlFlow<()> + 'a;

/// The hook that never stops a walk (what the functions without a hook pass).
pub fn no_hook(_: BlockStep) -> ControlFlow<()> {
    ControlFlow::Continue(())
}

/// Call `hook` with `done` of `total`; `true` when the walk must stop.
pub(crate) fn stop(hook: &mut BlockHook<'_>, done: u64, total: u64) -> bool {
    hook(BlockStep { done, total }).is_break()
}
