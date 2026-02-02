use std::collections::{HashMap, VecDeque};

use crate::config::stack_config::SharedConfig;
use crate::saps::sapmsg::SapMsg;
use crate::common::tdma_time::TdmaTime;
use crate::common::tetra_entities::TetraEntity;
use crate::entities::TetraEntityTrait;

use crossbeam_channel::Receiver;
use tokio::sync::broadcast;
use crate::gateway::{TxRequest, GatewayEvent};


#[derive(Default)]
pub enum MessagePrio {
    Immediate,
    #[default]
    Normal,
}

pub struct MessageQueue {
    messages: VecDeque<SapMsg>,
}

impl MessageQueue {
    pub fn new() -> Self {
        Self {
            messages: VecDeque::new(),
        }
    }

    pub fn push_back(&mut self, message: SapMsg) {
        self.messages.push_back(message);
    }

    pub fn push_prio(&mut self, message: SapMsg, prio: MessagePrio) {
        match prio {
            MessagePrio::Immediate => {
                // Insert at the front for immediate processing
                self.messages.push_front(message);
            }
            MessagePrio::Normal => {
                // Insert at the back for normal processing
                self.messages.push_back(message);
            }
        }
    }

    pub fn pop_front(&mut self) -> Option<SapMsg> {
        self.messages.pop_front()
    }
}

pub struct MessageRouter {
    /// While currently unused by the MessageRouter, this may change in the future
    /// As such, we provide the MessageRouter with a copy of the SharedConfig
    config: SharedConfig,
    entities: HashMap<TetraEntity, Box<dyn TetraEntityTrait>>,
    msg_queue: MessageQueue,

    /// The current TDMA time, if applicable. 
    /// For Bs mode, this is always available
    /// For Ms/Mon mode, it is recovered from a received SYNC frame and communicated in a different way
    ts: Option<TdmaTime>,
    gateway_rx: Option<Receiver<TxRequest>>,
    gateway_events: Option<broadcast::Sender<GatewayEvent>>,
}


impl MessageRouter {
    pub fn new(config: SharedConfig) -> Self {
        Self {
            entities: HashMap::new(),
            msg_queue: MessageQueue {
                messages: VecDeque::new(),
            },
            config,
            ts: None,
            gateway_rx: None,
            gateway_events: None,
        }
    }

    /// For BS mode, sets global TDMA time
    /// Incremented each tick and passed to entities in tick() function
    pub fn set_dl_time(&mut self, ts: TdmaTime) {        
        self.ts = Some(ts);
    }

    pub fn register_entity(&mut self, entity: Box<dyn TetraEntityTrait>) {
        let comp_type = entity.entity();
        tracing::debug!("register_entity {:?}", comp_type);
        self.entities.insert(comp_type, entity);
    }

    pub fn get_entity(&mut self, comp: TetraEntity) -> Option<&mut dyn TetraEntityTrait> {
        self.entities.get_mut(&comp).map(|entity| entity.as_mut())
    }

    pub fn submit_message(&mut self, message: SapMsg) {
        tracing::debug!("submit_message {:?}: {:?} -> {:?}", message.get_sap(), message.get_source(), message.get_dest());
        self.msg_queue.push_back(message);
    }

    fn deliver_one_due_or_rotate(&mut self, now: TdmaTime) -> bool {
        let Some(message) = self.msg_queue.pop_front() else {
            return false;
        };

        // If message is in the future, rotate it to the back and do NOT deliver.
        if message.dltime.diff(now) > 0 {
            self.msg_queue.push_back(message);
            return false;
        }

        tracing::debug!(
        "deliver_message: got {:?}: {:?} -> {:?} (dltime={}, now={})",
        message.get_sap(),
        message.get_source(),
        message.get_dest(),
        message.dltime,
        now
    );

        let dest = message.get_dest();
        if let Some(entity) = self.entities.get_mut(dest) {
            entity.rx_prim(&mut self.msg_queue, message);
        } else {
            tracing::warn!(
            "deliver_message: entity {:?} not found for {:?}: {:?} -> {:?}",
            dest,
            message.get_sap(),
            message.get_source(),
            message.get_dest()
        );
        }

        true
    }


    pub fn deliver_message(&mut self) {

        let message = self.msg_queue.pop_front();
        if let Some(message) = message {

            tracing::debug!("deliver_message: got {:?}: {:?} -> {:?}", message.get_sap(), message.get_source(), message.get_dest());

            // Determine the destination entity
            let dest = message.get_dest();

            // Check if the destination entity registered and deliver if found
            if let Some(entity) = self.entities.get_mut(dest) {
                entity.rx_prim(&mut self.msg_queue, message);
            } else {
                tracing::warn!("deliver_message: entity {:?} not found for {:?}: {:?} -> {:?}", dest, message.get_sap(), message.get_source(), message.get_dest());
            }
        }
    }

    pub fn deliver_all_messages(&mut self) {
        // If we don't have a TDMA time (MS/monitor mode), keep old behavior.
        let Some(now) = self.ts else {
            while !self.msg_queue.messages.is_empty() {
                self.deliver_message();
            }
            return;
        };

        // Deliver only messages that are due now.
        // Do multiple passes because delivering one message may enqueue more "due now" messages.
        loop {
            let n = self.msg_queue.messages.len();
            if n == 0 {
                break;
            }

            let mut progressed = false;

            for _ in 0..n {
                // Either delivers a due message, or rotates a future message to the back
                if self.deliver_one_due_or_rotate(now) {
                    progressed = true;
                }
            }

            // If in a whole pass we delivered nothing, everything remaining is in the future.
            if !progressed {
                break;
            }
        }
    }


    pub fn get_msgqueue_len(&self) -> usize {
        self.msg_queue.messages.len()
    }

    pub fn tick_all(&mut self) {
        
        if let Some(ts) = self.ts {
            // tracing::info!("--- tick dl {} ul {} txdl {} ----------------------------",
            //     ts, ts.add_timeslots(-2), ts.add_timeslots(MACSCHED_TX_AHEAD as i32));
            tracing::info!("--- tick dl {} ----------------------------", ts);
        } else {
            tracing::info!("---------------------------- tick ----------------------------");
        }

        self.pump_gateway_tx();


        // Call tick on all entities
        for entity in self.entities.values_mut() {
            entity.tick_start(&mut self.msg_queue, self.ts);
        }
    }

    /// Executes all end-of-tick functions:
    /// - LLC sends down all outstanding BL-ACKs
    /// - UMAC finalizes any resources for ts and sends down to LMAC
    /// 
    pub fn tick_end(&mut self) {

        tracing::debug!("############################ end-of-tick ############################");

        // Llc should send down outstanding BL-ACKs
        let target = TetraEntity::Llc;
        if let Some(entity) = self.entities.get_mut(&target) {
            tracing::trace!("tick_end for entity {:?}", target);
            entity.tick_end(&mut self.msg_queue, self.ts);
        }
        self.deliver_all_messages();

        // Umac should finalize any resources and send down to Lmac
        let target = TetraEntity::Umac;
        if let Some(entity) = self.entities.get_mut(&target) {
            tracing::trace!("tick_end for entity {:?}", target);
            entity.tick_end(&mut self.msg_queue, self.ts);
        }
        self.deliver_all_messages();

        // Then call tick_end on all other entities
        for entity in self.entities.values_mut() {
            let entity_id = entity.entity();
            if entity_id == TetraEntity::Llc || entity_id == TetraEntity::Umac {
                continue;
            }
            entity.tick_end(&mut self.msg_queue, self.ts);
        }
        self.deliver_all_messages();

        // Increment the TDMA time if set
        if let Some(ref mut ts) = self.ts {
            self.ts = Some(ts.add_timeslots(1));
        }
    }

    pub fn attach_gateway(
        &mut self,
        rx: Receiver<TxRequest>,
        events: broadcast::Sender<GatewayEvent>,
    ) {
        self.gateway_rx = Some(rx);
        self.gateway_events = Some(events);
    }

    fn pump_gateway_tx(&mut self) {
        let Some(now) = self.ts else { return; };

        // 1) Drain RX into a local vector WITHOUT calling any &mut self methods
        let mut drained: Vec<TxRequest> = Vec::new();
        if let Some(rx) = self.gateway_rx.as_ref() {
            drained.extend(rx.try_iter());
        }

        if drained.is_empty() {
            return;
        }

        // 2) Now we can freely mutate self while processing drained requests
        for req in drained {
            match crate::gateway::send::inject_text_sds(now, req.dst_ssi, &req.text) {
                Ok(msg) => {
                    self.submit_message(msg);

                    if let Some(events) = self.gateway_events.as_ref() {
                        let _ = events.send(GatewayEvent::TxQueued {
                            dst_ssi: req.dst_ssi,
                            text: req.text.clone(),
                        });
                    }
                }
                Err(e) => {
                    if let Some(events) = self.gateway_events.as_ref() {
                        let _ = events.send(GatewayEvent::TxFailed {
                            dst_ssi: req.dst_ssi,
                            reason: e.to_string(),
                        });
                    }
                }
            }
        }
    }




    /// Runs the full stack either forever or for a specified number of ticks.
    pub fn run_stack(&mut self, num_ticks: Option<u64>) {
        let mut ticks: u64 = 0;

        loop {
            self.tick_all();
            self.deliver_all_messages();
            self.tick_end();

            ticks += 1;
            if let Some(n) = num_ticks {
                if ticks >= n {
                    break;
                }
            }
        }
    }

}
