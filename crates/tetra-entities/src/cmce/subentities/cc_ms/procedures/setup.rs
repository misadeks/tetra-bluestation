use super::*;

impl CcMsSubentity {
    /// U-SETUP for MO group call (cl. 14.5.2.1.2; PDU cl. 14.7.2.10).
    pub fn originate_group_call(
        &mut self,
        queue: &mut MessageQueue,
        called_gssi: u32,
        basic_service: BasicServiceInformation,
        request_to_transmit: bool,
    ) {
        self.send_u_setup(
            queue,
            TetraAddress::new(called_gssi, SsiType::Gssi),
            basic_service,
            false,
            false,
            request_to_transmit,
        );
    }

    /// U-SETUP for MO individual call (cl. 14.5.6.2; PDU cl. 14.7.2.10).
    pub fn originate_individual_call(
        &mut self,
        queue: &mut MessageQueue,
        called_issi: u32,
        basic_service: BasicServiceInformation,
        simplex_duplex_selection: bool,
        request_to_transmit: bool,
    ) {
        self.send_u_setup(
            queue,
            TetraAddress::new(called_issi, SsiType::Issi),
            basic_service,
            true,
            simplex_duplex_selection,
            request_to_transmit,
        );
    }

    /// U-ALERT for the on/off-hook MT answer path (cl. 14.5.1.1.1; PDU cl.
    /// 14.7.2.1). Sent when the called user application accepts on/off-hook
    /// signalling (TNCC-SETUP response): CC signals that the called party is
    /// alerted and REMAINS in state MT-CALL-SETUP. Per the cl. 14.5.1.1.1
    /// bullets the called MS may here offer a different simplex/duplex or basic
    /// service — we faithfully pass through the values carried in the
    /// TNCC-SETUP response, falling back to the stored call state; no
    /// service-downgrade logic is invented.
    pub fn send_u_alert(&mut self, queue: &mut MessageQueue, call_identifier: u16, response: &tncc::TnccSetupResponse) -> bool {
        let Some(call) = self.calls.get(&call_identifier) else {
            return false;
        };
        let route = call.route;
        let basic_service = response
            .basic_service_information
            .as_ref()
            .and_then(|b| pdu_basic_from_tncc(b).ok())
            .unwrap_or_else(|| call.basic_service.clone());
        self.send_pdu(
            queue,
            &UAlert {
                call_identifier,
                // note 1 (PDU cl. 14.7.2.1): reserved, set to "1".
                reserved: true,
                simplex_duplex_selection: response.simplex_duplex_selection.as_bool(),
                basic_service_information: Some(basic_service),
                facility: None,
                proprietary: None,
            },
            route,
            false,
            false,
        );
        true
    }

    /// U-CONNECT for the MT answer path (cl. 14.5.1.1.1; PDU cl. 14.7.2.3).
    /// Direct set-up: sent on the TNCC-SETUP response. On/off-hook: sent on the
    /// TNCC-COMPLETE request once the local user has answered. In both cases CC
    /// starts timer T301 and REMAINS in state MT-CALL-SETUP until the
    /// D-CONNECT ACKNOWLEDGE PDU arrives (see `rx_d_connect_ack`).
    pub fn connect_call(
        &mut self,
        queue: &mut MessageQueue,
        call_identifier: u16,
        simplex_duplex_selection: bool,
        basic_service: BasicServiceInformation,
    ) -> bool {
        let now = self.dltime;
        let Some(call) = self.calls.get(&call_identifier) else {
            return false;
        };
        let route = call.route;
        // Reflect the negotiated signalling mode in the Hook method IE (cl.
        // 14.8.23): direct set-up → 0; on/off-hook → 1 (and, per cl. 14.5.1.1.1,
        // this IE is how a called MS unable to support direct set-up would offer
        // on/off-hook, or vice versa).
        let hook_method_selection = call.hook_on_off;
        self.send_pdu(
            queue,
            &UConnect {
                call_identifier,
                hook_method_selection,
                simplex_duplex_selection,
                basic_service_information: Some(basic_service),
                facility: None,
                proprietary: None,
            },
            route,
            false,
            false,
        );
        // Start T301 (cl. 14.5.1.1.1; timer cl. 14.5.1.3.4 a, maximum 30 s).
        // D-SETUP carries no Call time-out set-up phase IE, so arm from any
        // value already provisioned (codeplug predefined or a D-INFO update,
        // cl. 14.8.17); the predefined value is not invented without a codeplug
        // value, matching the MO setup-timer treatment.
        if let Some(call) = self.calls.get_mut(&call_identifier) {
            let setup_timeout = call.timers.setup_timeout.unwrap_or(CallTimeoutSetupPhase::Predefined);
            call.start_setup_timer(now, setup_timeout);
        }
        true
    }
}
