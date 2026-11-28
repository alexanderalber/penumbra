//! Which node the calling thread is cooking. Work recorded deeper down (GPU commands, say) asks
//! here whom it is for, so the cost can be put down to that node without every operator
//! passing its id along.

use std::cell::Cell;

use crate::NodeId;

thread_local! {
    static NODE: Cell<Option<NodeId>> = const { Cell::new(None) };
}

/// The node this thread is cooking, `None` outside a cook (or on a thread a cook started).
pub fn node() -> Option<NodeId> {
    NODE.with(Cell::get)
}

/// Marks this thread as cooking `node` until the guard drops. Guards nest: a node cooking another
/// one (a component's inner network) hands back to the outer node at the inner guard's end.
pub fn enter(node: NodeId) -> Cooking {
    Cooking { outer: NODE.with(|n| n.replace(Some(node))) }
}

/// Runs `f` as if this thread cooked no node: for work a cook triggers that is nobody's own (a
/// shared resource made on first use).
pub fn outside<T>(f: impl FnOnce() -> T) -> T {
    let outer = NODE.with(|n| n.replace(None));
    let _back = Cooking { outer };
    f()
}

/// The scope of [`enter`].
pub struct Cooking {
    outer: Option<NodeId>,
}

impl Drop for Cooking {
    fn drop(&mut self) {
        NODE.with(|n| n.set(self.outer));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_nest_and_end() {
        assert_eq!(node(), None);
        {
            let _outer = enter(NodeId(1));
            {
                let _inner = enter(NodeId(2));
                assert_eq!(node(), Some(NodeId(2)));
            }
            assert_eq!(node(), Some(NodeId(1)));
            assert_eq!(std::thread::spawn(node).join().ok(), Some(None), "a new thread cooks nothing");
        }
        assert_eq!(node(), None);
        let _a = enter(NodeId(3));
        assert_eq!(outside(node), None);
        assert_eq!(node(), Some(NodeId(3)));
    }
}
