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

    /// U-ALERT/U-CONNECT answer path for MT individual calls (cl. 14.5.6.5).
    pub fn answer_call(&mut self, queue: &mut MessageQueue, call_identifier: u16, alert_first: bool) -> bool {
        let Some(call) = self.calls.get(&call_identifier) else {
            return false;
        };
        let route = call.route;
        if alert_first {
            self.send_pdu(
                queue,
                &UAlert {
                    call_identifier,
                    reserved: true,
                    simplex_duplex_selection: call.simplex_duplex_selection,
                    basic_service_information: Some(call.basic_service.clone()),
                    facility: None,
                    proprietary: None,
                },
                route,
                false,
                false,
            );
        }
        self.send_pdu(
            queue,
            &UConnect {
                call_identifier,
                hook_method_selection: false,
                simplex_duplex_selection: call.simplex_duplex_selection,
                basic_service_information: Some(call.basic_service.clone()),
                facility: None,
                proprietary: None,
            },
            route,
            false,
            false,
        );
        true
    }
}
