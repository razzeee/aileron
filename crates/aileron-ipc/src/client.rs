use anyhow::Result;
use std::sync::{Arc, RwLock};
use varlink::Connection;

use crate::varlink_address;

/// Open a Varlink connection to the aileron daemon.
/// Returns an `Arc<RwLock<Connection>>` as required by the generated `VarlinkClient::new`.
pub fn connect() -> Result<Arc<RwLock<Connection>>> {
    let addr = varlink_address();
    let conn = Connection::with_address(&addr)?;
    Ok(conn)
}
