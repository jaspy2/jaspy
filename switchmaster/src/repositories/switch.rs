use diesel::pg::PgConnection;
use diesel::prelude::*;
use kerosiini::managed_pool::Pool;
use kerosiini::repository::{Repository, RepositoryError};
use kerosiini::pagination::*;
use r2d2;
use r2d2_diesel::ConnectionManager;
use rocket::http::Status;
use rocket::request::{self, FromRequest};
use rocket::{Outcome, Request, State};

use super::super::models::switch::*;
use super::super::schema::*;

pub struct SwitchRepository {
    connection: r2d2::PooledConnection<ConnectionManager<PgConnection>>,
}

impl<'a, 'r> FromRequest<'a, 'r> for SwitchRepository {
    type Error = ();

    fn from_request(request: &'a Request<'r>) -> request::Outcome<SwitchRepository, ()> {
        let pool = request.guard::<State<Pool>>()?;
        match pool.get() {
            Ok(conn) => Outcome::Success(SwitchRepository { connection: conn }),
            Err(_) => Outcome::Failure((Status::ServiceUnavailable, ())),
        }
    }
}

impl Repository<i64, NewSwitch, UpdatedSwitch, Switch> for SwitchRepository {
    fn paginate(self, page: i64, page_size: i64) -> Result<PaginatedQueryResult<Switch>, RepositoryError> {
        match switches::table.count().get_result(&*self.connection) {
            Ok(total) => {
                let records = switches::table
                    .select(switches::all_columns)
                    .limit(page_size)
                    .offset((page - 1) * page_size)
                    .load::<Switch>(&*self.connection)
                    .unwrap();
                    
                Ok(PaginatedQueryResult {
                    records: records,
                    page: page,
                    page_size: page_size,
                    total: total
                })
            },
            Err(err) => {
                Err(RepositoryError::from(err))
            }
        }
    }

    fn find(self, id: i64) -> Result<Switch, RepositoryError> {
        switches::table
            .filter(switches::id.eq(id))
            .first::<Switch>(&*self.connection)
            .map_err(|err| RepositoryError::from(err))
    }

    fn add(self, entry: NewSwitch) -> Result<Switch, RepositoryError> {
        diesel::insert_into(switches::table)
            .values(&entry)
            .get_result(&*self.connection)
            .map_err(|err| RepositoryError::from(err))
    }

    fn update(self, id: i64, entry: UpdatedSwitch) -> Result<usize, RepositoryError> {
        diesel::update(switches::table.find(id))
            .set(entry)
            .execute(&*self.connection)
            .map_err(|err| RepositoryError::from(err))
    }

    fn delete(self, id: i64) -> Result<Switch, RepositoryError> {
        diesel::delete(switches::table.find(id))
            .get_result(&*self.connection)
            .map_err(|err| RepositoryError::from(err))
    }
}
