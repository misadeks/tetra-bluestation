use as_any::AsAny;
use crate::common::messagerouter::MessageQueue;
use crate::saps::sapmsg::SapMsg;
use crate::config::stack_config::SharedConfig;
use crate::common::tdma_time::TdmaTime;
use crate::common::tetra_entities::TetraEntity;

pub mod phy;
pub mod lmac;
pub mod umac;
pub mod llc;
pub mod mle;
pub mod mm;
pub mod cmce;
pub mod sndcp;

pub trait TetraEntityTrait: Send + AsAny {
    fn entity(&self) -> TetraEntity;
    fn rx_prim(&mut self, queue: &mut MessageQueue, message: SapMsg);
    #[allow(dead_code)]
    fn set_config(&mut self, _config: SharedConfig) {}
    fn tick_start(&mut self, _queue: &mut MessageQueue, _ts: Option<TdmaTime>) { }
    fn tick_end(&mut self, _queue: &mut MessageQueue, _ts: Option<TdmaTime>) -> bool { false }
}

pub mod pei_at;

pub mod cmd_server;

pub mod http_ui;

pub mod wap_gateway;
