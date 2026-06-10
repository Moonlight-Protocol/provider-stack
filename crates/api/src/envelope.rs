//! Success response envelope: `{ "data": <payload> }`.
//!
//! Every successful handler wraps its payload in `Data` before serialising so the wire
//! shape matches the Deno reference's `{ data: ... }` convention. Tests read fields under
//! `data.foo`, never at the top level.

use serde::Serialize;

#[derive(Serialize)]
pub struct Data<T> {
    pub data: T,
}

impl<T> Data<T> {
    pub fn new(data: T) -> Self {
        Self { data }
    }
}
