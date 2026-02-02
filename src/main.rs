#![allow(dead_code)]

#[cfg(test)]
mod testing;
mod config;
mod common;
mod entities;
mod saps;
 
use clap::Parser;

use common::debug::setup_logging_default;
use common::tdma_time::TdmaTime;
use common::messagerouter::MessageRouter;
use config::stack_config::*;
use config::toml_config;
use crate::entities::cmce::cmce_bs::CmceBs;
use crate::entities::mle::mle_bs_ms::Mle;
use crate::entities::phy::components::soapy_dev::RxTxDevSoapySdr;
use crate::entities::sndcp::sndcp_bs::Sndcp;
use crate::entities::lmac::lmac_bs::LmacBs;
use crate::entities::mm::mm_bs::MmBs;
use crate::entities::phy::phy_bs::PhyBs;
use crate::entities::llc::llc_bs_ms::Llc;
use crate::entities::umac::umac_bs::UmacBs;
use crate::entities::pei_at::PeiAt;
use crate::entities::cmd_server::CmdServer;
use crate::entities::http_ui::HttpUi;

/// Load configuration file
fn load_config_from_toml(cfg_path: &str) -> SharedConfig {
    match toml_config::from_file(cfg_path) {
        Ok(c) => c,
        Err(e) => {
            println!("Failed to load configuration from {}: {}", cfg_path, e);
            std::process::exit(1);
        }
    }
}

/// Start base station stack
fn build_bs_stack(cfg: &mut SharedConfig) -> MessageRouter {

    let mut router = MessageRouter::new(cfg.clone());

    // Add suitable Phy component based on PhyIo type
    match cfg.config().phy_io.backend {
        PhyBackend::SoapySdr => {
            let rxdev = RxTxDevSoapySdr::new(cfg);
            let phy = PhyBs::new(cfg.clone(), rxdev);
            router.register_entity(Box::new(phy));
        } 
        _ => {
            panic!("Unsupported PhyIo type: {:?}", cfg.config().phy_io.backend);
        }
    }
    
    // Add remaining components
    let lmac = LmacBs::new(cfg.clone());
    let umac = UmacBs::new(cfg.clone());
    let llc = Llc::new(cfg.clone());
    let mle = Mle::new(cfg.clone());
    let mm = MmBs::new(cfg.clone());
    let sndcp = Sndcp::new(cfg.clone());
    let cmce = CmceBs::new(cfg.clone());
    let pei_at = PeiAt::new();
    let cmd_server = CmdServer::new();
    let http_ui = HttpUi::new();
    router.register_entity(Box::new(lmac));
    router.register_entity(Box::new(umac));
    router.register_entity(Box::new(llc));
    router.register_entity(Box::new(mle));
    router.register_entity(Box::new(mm));
    router.register_entity(Box::new(sndcp));
    router.register_entity(Box::new(cmce));
    router.register_entity(Box::new(pei_at));
    router.register_entity(Box::new(cmd_server));
    router.register_entity(Box::new(http_ui));
    
    // Init network time
    router.set_dl_time(TdmaTime::default());

    router
}


#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "TETRA BlueStation Stack",
    long_about = "Runs the TETRA BlueStation stack using the provided TOML configuration files"
)]


struct Args {
    /// Config file (required)
    #[arg(
        help = "TOML config with network/cell parameters",
    )]
    config: String,
}

fn main() {

    eprintln!("░▀█▀░█▀▀░▀█▀░█▀▄░█▀█░░░░░█▀▄░█░░░█░█░█▀▀░█▀▀░▀█▀░█▀█░▀█▀░▀█▀░█▀█░█▀█");
    eprintln!("░░█░░█▀▀░░█░░█▀▄░█▀█░▄▄▄░█▀▄░█░░░█░█░█▀▀░▀▀█░░█░░█▀█░░█░░░█░░█░█░█░█");
    eprintln!("░░▀░░▀▀▀░░▀░░▀░▀░▀░▀░░░░░▀▀░░▀▀▀░▀▀▀░▀▀▀░▀▀▀░░▀░░▀░▀░░▀░░▀▀▀░▀▀▀░▀░▀\n");
    eprintln!("    Wouter Bokslag / Midnight Blue");
    eprintln!(" -> https://github.com/MidnightBlueLabs/tetra-bluestation");
    eprintln!(" -> https://midnightblue.nl\n");

    let args = Args::parse();
    let mut cfg = load_config_from_toml(&args.config);
    let _log_guard = setup_logging_default(cfg.config().debug_log.clone());
    
    let mut router = match cfg.config().stack_mode {
        StackMode::Mon => {
            unimplemented!("Monitor mode is not implemented");
        },
        StackMode::Ms => {
            unimplemented!("MS mode is not implemented");
        },
        StackMode::Bs => {
            build_bs_stack(&mut cfg)
        }
    };

    router.run_stack(None);
}
