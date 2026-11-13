use serde::Deserialize;
use utoipa::IntoParams;

#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[derive(Deserialize)]
pub struct BlockRangeQuery {
    #[param(minimum = 0)]
    pub slot_from: usize,
    #[param(minimum = 0)]
    pub slot_to: usize,
}
