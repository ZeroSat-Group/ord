use super::*;
use jsonrpsee_server::types::{ErrorObject, ErrorObjectOwned};

#[derive(Debug)]
pub(crate) enum RpcServerError {
  Internal(Error),
  #[allow(dead_code)]
  BadRequest(String),
  NotFound(String),
}

impl Display for RpcServerError {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    match self {
      RpcServerError::Internal(error) => {
        write!(f, "error serving request: {}", error)
      }
      RpcServerError::BadRequest(msg) => {
        write!(f, "error bad request: {}", msg)
      }
      RpcServerError::NotFound(msg) => {
        write!(f, "error not found: {}", msg)
      }
    }
  }
}

impl From<Error> for RpcServerError {
  fn from(error: Error) -> Self {
    Self::Internal(error)
  }
}

pub(super) type RpcServerResult<T> = Result<T, RpcServerError>;

pub(super) trait OptionExt<T> {
  fn ok_or_not_found<F: FnOnce() -> S, S: Into<String>>(self, f: F) -> RpcServerResult<T>;
}

impl<T> OptionExt<T> for Option<T> {
  fn ok_or_not_found<F: FnOnce() -> S, S: Into<String>>(self, f: F) -> RpcServerResult<T> {
    match self {
      Some(value) => Ok(value),
      None => Err(RpcServerError::NotFound(f().into() + " not found")),
    }
  }
}

impl From<RpcServerError> for ErrorObjectOwned {
  fn from(err: RpcServerError) -> Self {
    ErrorObject::owned(-32000, format!("RpcServerError: {}", err), None::<()>)
  }
}

impl From<jsonrpsee_core::error::Error> for RpcServerError {
  fn from(value: jsonrpsee_core::Error) -> Self {
    Self::Internal(Error::msg(value))
  }
}
