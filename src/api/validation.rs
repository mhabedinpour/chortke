use crate::api::error::Error;
use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
use axum_valid::{ValidationRejection, Validified};
use validify::ValidationErrors;

pub struct ValidatedJson<T>(pub T);

impl<S, T> FromRequest<S> for ValidatedJson<T>
where
    S: Send + Sync,
    Validified<Json<T>>:
        FromRequest<S, Rejection = ValidationRejection<ValidationErrors, JsonRejection>>,
{
    type Rejection = Error;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Validified::<Json<T>>::from_request(req, state).await {
            Ok(Validified(Json(value))) => Ok(ValidatedJson(value)),
            Err(ValidationRejection::Valid(validation_errors)) => {
                Err(Error::Validation(validation_errors))
            }
            Err(ValidationRejection::Inner(json_rejection)) => Err(Error::BadRequest(
                "INVALID_JSON".to_string(),
                json_rejection.to_string(),
            )),
        }
    }
}
