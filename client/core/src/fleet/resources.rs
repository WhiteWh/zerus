//! Fixed source admission for an accounted Fleet. A retired source keeps its
//! slot through its stop callback and actual supervisor/pipe resources.
use crate::{
    desktop::drain::{Charge, Gate, Role},
    stream::StreamHandle,
};
use std::{ops::Deref, sync::Arc};
#[derive(Clone)]
pub(super) struct Graph {
    pub host: Arc<dyn Send + Sync>,
    slots: Gate,
}
struct Source {
    _host: Arc<dyn Send + Sync>,
    _slot: Charge,
}
pub(super) struct Stream {
    pub handle: StreamHandle,
    pub resources: Option<Arc<dyn Send + Sync>>,
}
impl Graph {
    pub fn new(host: Arc<dyn Send + Sync>) -> Self {
        Self {
            host,
            slots: Gate::new(crate::source::SOURCES).unwrap(),
        }
    }
    pub fn admit(&self) -> Option<Arc<dyn Send + Sync>> {
        Some(Arc::new(Source {
            _host: self.host.clone(),
            _slot: self.slots.admit(Role::Fleet).ok()?,
        }))
    }
    pub fn available(&self) -> bool {
        self.slots.snapshot().is_ok()
    }
    pub fn close(&self) {
        self.slots.close();
    }
}
impl Stream {
    pub fn stop(self) {
        let Self {
            handle,
            resources: _retirement,
        } = self;
        handle.stop();
    }
}
impl Deref for Stream {
    type Target = StreamHandle;
    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::drain::Observation;
    use std::time::Instant;

    #[test]
    fn retired_source_slots_stay_bounded_until_their_last_actual_resource_drops() {
        let host = Gate::new(1).unwrap();
        let graph = Graph::new(Arc::new(host.admit(Role::Fleet).unwrap()));
        let sources: Vec<_> = (0..crate::source::SOURCES)
            .map(|_| graph.admit().unwrap())
            .collect();
        assert!(graph.admit().is_none());
        let retired = sources[0].clone();
        drop(sources);
        let rest: Vec<_> = (1..crate::source::SOURCES)
            .map(|_| graph.admit().unwrap())
            .collect();
        assert!(
            graph.admit().is_none(),
            "retired resources still cost a slot"
        );
        graph.close();
        drop(rest);
        assert!(graph.admit().is_none(), "close never reopens");
        drop(graph);
        let closed = host.close();
        assert_eq!(closed.wait_until(Instant::now()), Observation::Pending);
        drop(retired);
        assert_eq!(closed.wait_until(Instant::now()), Observation::Idle);
    }
}
