/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use crossbeam_channel::{select, Receiver, Sender};
use devtools_traits::DevtoolScriptControlMsg;

use crate::dom::abstractworker::WorkerScriptMsg;
use crate::dom::bindings::conversions::DerivedFrom;
use crate::dom::bindings::reflector::DomObject;
use crate::dom::dedicatedworkerglobalscope::{AutoWorkerReset, DedicatedWorkerScriptMsg};
use crate::dom::globalscope::GlobalScope;
use crate::dom::worker::TrustedWorkerAddress;
use crate::dom::workerglobalscope::WorkerGlobalScope;
use crate::realms::enter_realm;
use crate::script_runtime::{CanGc, CommonScriptMsg, ScriptChan, ScriptPort};
use crate::task_queue::{QueuedTaskConversion, TaskQueue};

/// A ScriptChan that can be cloned freely and will silently send a TrustedWorkerAddress with
/// common event loop messages. While this SendableWorkerScriptChan is alive, the associated
/// Worker object will remain alive.
#[derive(Clone, JSTraceable)]
pub struct SendableWorkerScriptChan {
    #[no_trace]
    pub sender: Sender<DedicatedWorkerScriptMsg>,
    pub worker: TrustedWorkerAddress,
}

impl ScriptChan for SendableWorkerScriptChan {
    fn send(&self, msg: CommonScriptMsg) -> Result<(), ()> {
        let msg = DedicatedWorkerScriptMsg::CommonWorker(
            self.worker.clone(),
            WorkerScriptMsg::Common(msg),
        );
        self.sender.send(msg).map_err(|_| ())
    }

    fn as_boxed(&self) -> Box<dyn ScriptChan> {
        Box::new(SendableWorkerScriptChan {
            sender: self.sender.clone(),
            worker: self.worker.clone(),
        })
    }
}

/// A ScriptChan that can be cloned freely and will silently send a TrustedWorkerAddress with
/// worker event loop messages. While this SendableWorkerScriptChan is alive, the associated
/// Worker object will remain alive.
#[derive(Clone, JSTraceable)]
pub struct WorkerThreadWorkerChan {
    #[no_trace]
    pub sender: Sender<DedicatedWorkerScriptMsg>,
    pub worker: TrustedWorkerAddress,
}

impl ScriptChan for WorkerThreadWorkerChan {
    fn send(&self, msg: CommonScriptMsg) -> Result<(), ()> {
        let msg = DedicatedWorkerScriptMsg::CommonWorker(
            self.worker.clone(),
            WorkerScriptMsg::Common(msg),
        );
        self.sender.send(msg).map_err(|_| ())
    }

    fn as_boxed(&self) -> Box<dyn ScriptChan> {
        Box::new(WorkerThreadWorkerChan {
            sender: self.sender.clone(),
            worker: self.worker.clone(),
        })
    }
}

impl ScriptPort for Receiver<DedicatedWorkerScriptMsg> {
    fn recv(&self) -> Result<CommonScriptMsg, ()> {
        let common_msg = match self.recv() {
            Ok(DedicatedWorkerScriptMsg::CommonWorker(_worker, common_msg)) => common_msg,
            Err(_) => return Err(()),
            Ok(DedicatedWorkerScriptMsg::WakeUp) => panic!("unexpected worker event message!"),
        };
        match common_msg {
            WorkerScriptMsg::Common(script_msg) => Ok(script_msg),
            WorkerScriptMsg::DOMMessage { .. } => panic!("unexpected worker event message!"),
        }
    }
}

pub trait WorkerEventLoopMethods {
    type WorkerMsg: QueuedTaskConversion + Send;
    type ControlMsg;
    type Event;
    fn task_queue(&self) -> &TaskQueue<Self::WorkerMsg>;
    fn handle_event(&self, event: Self::Event, can_gc: CanGc) -> bool;
    fn handle_worker_post_event(&self, worker: &TrustedWorkerAddress) -> Option<AutoWorkerReset>;
    fn from_control_msg(msg: Self::ControlMsg) -> Self::Event;
    fn from_worker_msg(msg: Self::WorkerMsg) -> Self::Event;
    fn from_devtools_msg(msg: DevtoolScriptControlMsg) -> Self::Event;
    fn from_timer_msg() -> Self::Event;
    fn control_receiver(&self) -> &Receiver<Self::ControlMsg>;
}

// https://html.spec.whatwg.org/multipage/#worker-event-loop
pub fn run_worker_event_loop<T, WorkerMsg, Event>(
    worker_scope: &T,
    worker: Option<&TrustedWorkerAddress>,
    can_gc: CanGc,
) where
    WorkerMsg: QueuedTaskConversion + Send,
    T: WorkerEventLoopMethods<WorkerMsg = WorkerMsg, Event = Event>
        + DerivedFrom<WorkerGlobalScope>
        + DerivedFrom<GlobalScope>
        + DomObject,
{
    let scope = worker_scope.upcast::<WorkerGlobalScope>();
    let task_queue = worker_scope.task_queue();

    let never = crossbeam_channel::never();
    let devtools_receiver = scope.devtools_receiver().unwrap_or(&never);

    let event = select! {
        recv(worker_scope.control_receiver()) -> msg => T::from_control_msg(msg.unwrap()),
        recv(task_queue.select()) -> msg => {
            task_queue.take_tasks(msg.unwrap());
            T::from_worker_msg(task_queue.recv().unwrap())
        },
        recv(devtools_receiver) -> msg => T::from_devtools_msg(msg.unwrap()),
        recv(scope.timer_scheduler().wait_channel()) -> _ => T::from_timer_msg(),
    };

    scope.timer_scheduler().dispatch_completed_timers();

    let mut sequential = vec![event];

    // https://html.spec.whatwg.org/multipage/#worker-event-loop
    // Once the WorkerGlobalScope's closing flag is set to true,
    // the event loop's task queues must discard any further tasks
    // that would be added to them
    // (tasks already on the queue are unaffected except where otherwise specified).
    while !scope.is_closing() {
        // Batch all events that are ready.
        // The task queue will throttle non-priority tasks if necessary.
        match task_queue.take_tasks_and_recv() {
            Err(_) => match devtools_receiver.try_recv() {
                Ok(message) => sequential.push(T::from_devtools_msg(message)),
                Err(_) => break,
            },
            Ok(ev) => sequential.push(T::from_worker_msg(ev)),
        }
    }

    // Step 3
    for event in sequential {
        let _realm = enter_realm(worker_scope);
        if !worker_scope.handle_event(event, can_gc) {
            // Shutdown
            return;
        }
        // Step 6
        let _ar = match worker {
            Some(worker) => worker_scope.handle_worker_post_event(worker),
            None => None,
        };
        worker_scope
            .upcast::<GlobalScope>()
            .perform_a_microtask_checkpoint(can_gc);
    }
    worker_scope
        .upcast::<GlobalScope>()
        .perform_a_dom_garbage_collection_checkpoint();
}
