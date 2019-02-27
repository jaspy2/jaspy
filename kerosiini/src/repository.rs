use std::error;
use std::fmt;
use rocket::response::Responder;
use rocket::{Request, Response};
use rocket::http::{Status, ContentType};
use super::pagination::PaginatedQueryResult;
use diesel::result::Error as DieselError;

#[derive(Debug)]
pub enum RepositoryError {
    DatabaseLayerError(DieselError),
    InvalidModelState,
    NotFound
}

impl error::Error for RepositoryError {
    fn description(&self) -> &str {
        match *self {
            RepositoryError::DatabaseLayerError(ref _err) => "And error has occured during a database operation",
            RepositoryError::InvalidModelState => "The given data model failed validation",
            RepositoryError::NotFound => "The requested entry was not found"
        }
    }

    fn cause(&self) -> Option<&error::Error> {
        match *self {
            RepositoryError::DatabaseLayerError(ref err) => Some(err),
            RepositoryError::InvalidModelState => None,
            RepositoryError::NotFound => None
        }
    }
}

impl From<DieselError> for RepositoryError {
    fn from(err: DieselError) -> Self {
        if err == DieselError::NotFound {
            RepositoryError::NotFound
        } else {
            RepositoryError::DatabaseLayerError(err)
        }
    }
}

impl fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self)
    }
}

impl Responder<'static> for RepositoryError {
    fn respond_to(self, _: &Request) -> std::result::Result<Response<'static>, Status> {
        match self {
            RepositoryError::NotFound => Response::build()
                .status(Status::NotFound)
                .ok(),
            _ => Response::build()
                .header(ContentType::JSON)
                .status(Status::InternalServerError)
                .ok()
        }
    }
}

pub trait Repository<TKey, TNew, TUpdate, TResult> {
    fn paginate(self, page: i64, page_size: i64) -> Result<PaginatedQueryResult<TResult>, RepositoryError>;
    fn find(self, id: TKey) -> Result<TResult, RepositoryError>;

    fn add(self, entry: TNew) -> Result<TResult, RepositoryError>;
    fn update(self, id: TKey, entry: TUpdate) -> Result<usize, RepositoryError>;
    fn delete(self, id: TKey) -> Result<TResult, RepositoryError>;
}
