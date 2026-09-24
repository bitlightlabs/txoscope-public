use txoscope_core::backend::{
	OperationSelection, PlannedExactOutpointsRequest, PlannedSpecificOutpointRequest,
};

use crate::{PlanExactOutpointsRequest, PlannedSelection, ReserveSpecificUtxoRequest};

impl From<OperationSelection> for PlannedSelection {
	fn from(selection: OperationSelection) -> Self {
		Self {
			operation_id: selection.operation_id,
			selected_outpoints: selection.selected_outpoints,
			change_scope_id: selection.change_scope_id,
			expires_at: selection.expires_at,
		}
	}
}

impl From<PlanExactOutpointsRequest> for PlannedExactOutpointsRequest {
	fn from(req: PlanExactOutpointsRequest) -> Self {
		Self::new(req.operation_id, req.target_account_id, req.selected_outpoints)
	}
}

impl From<ReserveSpecificUtxoRequest> for PlannedSpecificOutpointRequest {
	fn from(req: ReserveSpecificUtxoRequest) -> Self {
		let request = PlannedSpecificOutpointRequest::new(
			req.operation_id,
			req.target_account_id,
			req.outpoint,
		);
		match req.expires_at {
			Some(expires_at) => request.expires_at(expires_at),
			None => request,
		}
	}
}
