use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tetra_config::bluestation::{SharedConfig, StackMode};
use tetra_core::{TdmaTime, tetra_entities::TetraEntity};
use tetra_saps::SapMsg;

use crate::TetraEntityTrait;

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
        Self { messages: VecDeque::new() }
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

    /// Iterate over the queued messages without consuming them.
    pub fn iter(&self) -> impl Iterator<Item = &SapMsg> {
        self.messages.iter()
    }
}

pub struct MessageRouter {
    /// While currently unused by the MessageRouter, this may change in the future
    /// As such, we provide the MessageRouter with a copy of the SharedConfig
    _config: SharedConfig,
    entities: HashMap<TetraEntity, Box<dyn TetraEntityTrait>>,
    msg_queue: MessageQueue,

    /// The current TDMA time, if applicable.
    /// For Bs mode, this is always available
    /// For Ms/Mon mode, it is recovered from a received SYNC frame and communicated in a different way
    ts: TdmaTime,
}

impl MessageRouter {
    pub fn new(config: SharedConfig) -> Self {
        Self {
            entities: HashMap::new(),
            msg_queue: MessageQueue { messages: VecDeque::new() },
            _config: config,
            ts: TdmaTime::default(),
        }
    }

    /// For BS mode, sets global TDMA time
    /// Incremented each tick and passed to entities in tick() function
    pub fn set_dl_time(&mut self, ts: TdmaTime) {
        self.ts = ts;
    }

    pub fn register_entity(&mut self, entity: Box<dyn TetraEntityTrait>) {
        let comp_type = entity.entity();
        tracing::debug!("register_entity {:?}", comp_type);
        self.entities.insert(comp_type, entity);
    }

    /// Returns a mut ref to a component of the requested type
    pub fn get_entity(&mut self, comp: TetraEntity) -> Option<&mut dyn TetraEntityTrait> {
        self.entities.get_mut(&comp).map(|entity| entity.as_mut())
    }

    pub fn submit_message(&mut self, message: SapMsg) {
        tracing::debug!(
            "submit_message {:?}: {:?} -> {:?}",
            message.get_sap(),
            message.get_source(),
            message.get_dest()
        );
        self.msg_queue.push_back(message);
    }

    pub fn deliver_message(&mut self) {
        let message = self.msg_queue.pop_front();
        if let Some(message) = message {
            tracing::debug!(
                "deliver_message: got {:?}: {:?} -> {:?}",
                message.get_sap(),
                message.get_source(),
                message.get_dest()
            );

            // Determine the destination entity
            let dest = message.get_dest();

            // Check if the destination entity registered and deliver if found
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
        }
    }

    pub fn deliver_all_messages(&mut self) {
        while !self.msg_queue.messages.is_empty() {
            self.deliver_message();
        }
    }

    pub fn get_msgqueue_len(&self) -> usize {
        self.msg_queue.messages.len()
    }

    pub fn tick_start(&mut self) {
        // tracing::info!("--- tick dl {} ul {} txdl {} ----------------------------",
        //     self.ts, self.ts.add_timeslots(-2), self.ts.add_timeslots(MACSCHED_TX_AHEAD as i32));
        tracing::info!("--- tick dl {} ----------------------------", self.ts);

        // Call tick on all entities
        for entity in self.entities.values_mut() {
            entity.tick_start(&mut self.msg_queue, self.ts);
        }
    }

    /// Executes all end-of-tick functions:
    /// - LLC sends down all outstanding BL-ACKs
    /// - UMAC finalizes any resources for ts and sends down to LMAC
    ///
    /// For BS mode (`increment_time = true`) the TDMA time is a free-running
    /// counter advanced one timeslot per tick. For RX-driven modes
    /// (`increment_time = false`) time is instead set from the recovered
    /// downlink slot, so it must not be advanced here.
    fn run_tick_end(&mut self, increment_time: bool) {
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

        // Increment the TDMA time if set (BS mode only).
        if increment_time {
            self.ts = self.ts.add_timeslots(1);
        }
    }

    /// End-of-tick for BS mode: advances the free-running TDMA clock.
    pub fn tick_end(&mut self) {
        self.run_tick_end(true);
    }

    /// Ask the MM entity to begin the MS de-registration procedure (ITSI detach,
    /// ETSI TS 100 392-2 cl. 16.6.1) at shutdown. Returns `true` if a detach was
    /// initiated and the stack should keep running (bounded) so the burst can be
    /// transmitted before the SDR closes. The queued PDU is delivered down the
    /// stack immediately so it is pending in the MAC for the next uplink
    /// opportunity.
    fn begin_deregistration(&mut self) -> bool {
        let started = match self.entities.get_mut(&TetraEntity::Mm) {
            Some(mm) => mm.begin_deregistration(&mut self.msg_queue),
            None => false,
        };
        if started {
            self.deliver_all_messages();
        }
        started
    }

    /// Returns `true` while a shutdown de-registration is still draining (the
    /// U-ITSI DETACH has not yet had time to be transmitted).
    fn deregistration_pending(&self) -> bool {
        self.entities
            .get(&TetraEntity::Mm)
            .map(|mm| mm.deregistration_pending())
            .unwrap_or(false)
    }

    /// Runs the full stack either forever or for a specified number of ticks.
    /// If `running` is provided, the loop will exit when the flag is set to false
    /// (e.g. by a Ctrl+C signal handler), allowing entities to be dropped cleanly.
    pub fn run_stack(&mut self, num_ticks: Option<usize>, running: Option<Arc<AtomicBool>>) {
        // MS mode is receive-timed: the PHY recovers the clock from the
        // downlink. BS mode is transmit-timed: the PHY's blocking TX call paces
        // the stack and the clock free-runs.
        if matches!(self._config.config().stack_mode, StackMode::Ms) {
            self.run_stack_ms(num_ticks, running);
        } else {
            self.run_stack_bs(num_ticks, running);
        }
    }

    /// BS run loop: the PHY's blocking `rxtx_timeslot` (driven each tick by the
    /// UMAC scheduler) paces the stack, and the TDMA clock free-runs.
    fn run_stack_bs(&mut self, num_ticks: Option<usize>, running: Option<Arc<AtomicBool>>) {
        let mut ticks: usize = 0;

        loop {
            // Check if we've been asked to stop (e.g. Ctrl+C)
            if let Some(ref flag) = running {
                if !flag.load(Ordering::Relaxed) {
                    eprintln!("\n[INFO] Shutting down gracefully...");
                    break;
                }
            }

            // Send tick_start event
            self.tick_start();

            // Deliver messages until queue empty
            while self.get_msgqueue_len() > 0 {
                self.deliver_all_messages();
            }

            // Send tick_end event and process final messages
            self.run_tick_end(true);

            // Check if we should stop
            ticks += 1;
            if let Some(num_ticks) = num_ticks {
                if ticks >= num_ticks {
                    break;
                }
            }
        }
    }

    /// MS run loop: the PHY blocks on the downlink and returns the TDMA time
    /// recovered from the received slot (ETSI TS 100 392-2 clause 7). That
    /// recovered time drives the stack clock; there is no free-running
    /// increment because RX advances time.
    fn run_stack_ms(&mut self, num_ticks: Option<usize>, running: Option<Arc<AtomicBool>>) {
        let mut ticks: usize = 0;
        // Set once the stop signal is seen and the de-registration (ITSI detach)
        // has been initiated; while true the loop keeps running (bounded) so the
        // detach burst can be transmitted before the SDR streams close.
        let mut detaching = false;
        // Hard safety cap on drain iterations: if the downlink is lost mid-drain
        // the MM countdown (driven from tick_start) stops advancing, so bound the
        // drain by loop iterations too and never hang on shutdown.
        let mut detach_iters: usize = 0;
        const MAX_DETACH_ITERS: usize = 2000;

        loop {
            // Check if we've been asked to stop (e.g. Ctrl+C)
            if let Some(ref flag) = running {
                if !flag.load(Ordering::Relaxed) {
                    if !detaching {
                        // First time we see the stop signal: try to de-register.
                        // If the MS is registered this sends a U-ITSI DETACH
                        // (cl. 16.6.1) and we keep running to transmit it;
                        // otherwise stop immediately.
                        if self.begin_deregistration() {
                            eprintln!(
                                "\n[INFO] De-registering (ITSI detach) before shutdown..."
                            );
                            detaching = true;
                        } else {
                            eprintln!("\n[INFO] Shutting down gracefully...");
                            break;
                        }
                    } else if !self.deregistration_pending()
                        || detach_iters >= MAX_DETACH_ITERS
                    {
                        // Detach has had time to be transmitted (or the drain hit
                        // its hard cap); stop now.
                        eprintln!("\n[INFO] De-registration sent; shutting down gracefully...");
                        break;
                    }
                    // else: still draining — fall through and keep running.
                    detach_iters += 1;
                }
            }

            // Block on the downlink until the PHY produces a demodulated slot.
            // The recovered TDMA time becomes the stack clock for this tick.
            let recovered = if let Some(phy) = self.entities.get_mut(&TetraEntity::Phy) {
                phy.drive_rx(&mut self.msg_queue)
            } else {
                None
            };

            // Run a full tick only when a downlink slot was recovered: the
            // recovered TDMA time is the stack clock and drives tick_start /
            // tick_end. While unsynchronized (no recovered slot, e.g. scanning
            // across an empty candidate carrier) there is no clock to tick.
            if let Some(ts) = recovered {
                self.ts = ts;
                self.tick_start();
            }

            // Always drain the message queue after driving the PHY, even when no
            // slot was recovered. While unsynchronized the PHY still emits
            // primitives that must propagate — the scan-dwell heartbeat
            // (TlmbScanDwellInd) to the MLE and the resulting cell-selection
            // retunes (TlmcTuneReq) down to the PHY — so the receiver can step
            // across candidate carriers before it is camped (cl. 18.3.4).
            while self.get_msgqueue_len() > 0 {
                self.deliver_all_messages();
            }

            if recovered.is_some() {
                // Send tick_end event without advancing the clock (RX drives time).
                self.run_tick_end(false);

                // Check if we should stop
                ticks += 1;
                if let Some(num_ticks) = num_ticks {
                    if ticks >= num_ticks {
                        break;
                    }
                }
            }
        }
    }
}
